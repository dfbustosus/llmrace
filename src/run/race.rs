//! The standard race (or sweep) mode.

use super::collect::race_collect;
use super::config::RunConfig;
use super::reports::{export_metrics, record_history, regression_gate, ttfb_gate, write_reports};
use anyhow::Result;
use llmrace::bench::LaneSummary;
use llmrace::report;

/// Runs one full race (or sweep) and returns the process exit code.
pub(crate) async fn run_race(cfg: RunConfig) -> Result<(i32, Vec<LaneSummary>)> {
    if !cfg.quiet {
        println!(
            "\u{1f3c1} llmrace v{} — {} providers × {} rounds ({} warmup, c={})",
            env!("CARGO_PKG_VERSION"),
            cfg.providers.len(),
            cfg.bench.runs,
            cfg.bench.warmup,
            cfg.bench.concurrency
        );
        println!("   prompt: \"{}\"\n", cfg.prompt.replace('\n', " "));
    }
    if cfg.bench.tools {
        println!(
            "\u{1f6e0} tool-calling mode: {} task, argument validity tracked",
            cfg.bench.tool_task
        );
    }

    // Reasoning models spend the token budget thinking first; a small cap
    // yields almost no visible output (live-verified: gpt-5-nano returned 1
    // visible token with --max-tokens 256).
    let reasoning_capped = cfg
        .providers
        .iter()
        .any(|p| llmrace::provider::is_reasoning_model(&p.default_model))
        && cfg.bench.max_tokens < 512;
    if reasoning_capped && !cfg.quiet {
        println!(
            "\u{1f9e0} note: reasoning models think before the first visible token — consider --max-tokens 512+ so the answer isn't starved"
        );
    }

    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let summaries = race_collect(&cfg, None, false, std::sync::Arc::clone(&stop)).await?;
    if stop.load(std::sync::atomic::Ordering::Relaxed) {
        eprintln!("\u{23f9} interrupted — writing partial results");
    }

    if !cfg.quiet {
        let table = report::terminal_table(&summaries);
        println!("\n{table}");
        if cfg.winner {
            let groups = llmrace::score::score_groups(&summaries, &cfg.weights);
            print!("{}", llmrace::score::podiums(&groups, &cfg.weights));
        }
    }

    write_reports(&summaries, &cfg)?;
    export_metrics(&summaries, &cfg).await?;
    if cfg.record {
        record_history(&summaries, &cfg.storage)?;
    }

    if cfg.notify_desktop && !summaries.is_empty() {
        let winner = summaries.iter().filter(|s| s.successful()).max_by(|a, b| {
            a.goodput_tokens_per_sec()
                .partial_cmp(&b.goodput_tokens_per_sec())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        if let Some(w) = winner {
            llmrace::notify::desktop_notification(
                "llmrace complete",
                &format!(
                    "{} won at {:.0} tok/s \u{2014} {} lanes raced",
                    w.name,
                    w.goodput_tokens_per_sec(),
                    summaries.len()
                ),
            );
        }
    }

    // A run where every lane failed must exit non-zero: the TTFB and
    // baseline gates only judge successful lanes and would otherwise
    // pass over a total outage.
    if llmrace::gates::all_lanes_failed(&summaries) {
        eprintln!("\n✗ every provider failed — no metrics were measured");
        return Ok((1, Vec::new()));
    }

    if !ttfb_gate(&summaries, &cfg) {
        return Ok((1, Vec::new()));
    }
    if !regression_gate(&summaries, &cfg)? {
        return Ok((1, Vec::new()));
    }
    Ok((0, summaries))
}
