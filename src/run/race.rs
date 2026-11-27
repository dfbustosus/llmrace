//! The standard race (or sweep) mode.

use super::collect::{race_collect, SignalGuard};
use super::config::{resolve_model, RunConfig};
use super::reports::{
    export_metrics, load_regression, record_history, regression_gate, ttfb_gate, write_reports,
};
use anyhow::Result;
use llmrace::bench::LaneSummary;
use llmrace::report;
use std::io::IsTerminal as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Runs one full race (or sweep) and returns the process exit code and summaries.
pub(crate) async fn run_race(cfg: RunConfig) -> Result<(i32, Vec<LaneSummary>)> {
    let stop = Arc::new(AtomicBool::new(false));
    let _signal_guard = (cfg.no_tui || !std::io::stdout().is_terminal()).then(|| {
        SignalGuard::with_flag(
            Arc::clone(&stop),
            "\u{23f9} interrupt received — finishing in-flight rounds, then writing reports…",
        )
    });
    run_race_with_stop(cfg, stop).await
}

/// Runs one full race (or sweep) sharing a caller-owned stop flag (used by
/// `--suite` so one [`SignalGuard`] governs the entire suite).
pub(crate) async fn run_race_with_stop(
    cfg: RunConfig,
    stop: Arc<AtomicBool>,
) -> Result<(i32, Vec<LaneSummary>)> {
    if !cfg.quiet {
        cfg.status(format!(
            "\u{1f3c1} llmrace v{} — {} providers × {} rounds ({} warmup, c={})",
            env!("CARGO_PKG_VERSION"),
            cfg.providers.len(),
            cfg.bench.runs,
            cfg.bench.warmup,
            cfg.bench.concurrency
        ));
        cfg.status(format!(
            "   prompt: \"{}\"\n",
            cfg.prompt.replace('\n', " ")
        ));
    }
    if cfg.bench.tools {
        cfg.status(format!(
            "\u{1f6e0} tool-calling mode: {} task, argument validity tracked",
            cfg.bench.tool_task
        ));
    }

    // Reasoning models spend the token budget thinking first; a small cap
    // yields almost no visible output (live-verified: gpt-5-nano returned 1
    // visible token with --max-tokens 256).
    let reasoning_capped = cfg.providers.iter().any(|p| {
        let model = resolve_model(&cfg.model_overrides, &p.id, &p.default_model);
        llmrace::provider::is_reasoning_model(&model)
    }) && cfg.bench.max_tokens < 512;
    if reasoning_capped && !cfg.quiet {
        cfg.status(
            "\u{1f9e0} note: reasoning models think before the first visible token — consider --max-tokens 512+ so the answer isn't starved",
        );
    }

    let summaries = race_collect(&cfg, None, false, Arc::clone(&stop)).await?;
    if stop.load(Ordering::Relaxed) {
        eprintln!("\u{23f9} interrupted — writing partial results");
    }

    if !cfg.quiet {
        let table = report::terminal_table(&summaries);
        cfg.status(format!("\n{table}"));
        if cfg.winner {
            let weights = llmrace::score::applied_weights(&summaries, &cfg.weights);
            let groups = llmrace::score::score_groups(&summaries, &weights);
            cfg.status_inline(llmrace::score::podiums(&groups, &weights));
        }
    }

    let loaded = load_regression(&summaries, &cfg);
    write_reports(
        &summaries,
        &cfg,
        loaded.as_ref().ok().and_then(|report| report.as_ref()),
    )?;
    export_metrics(&summaries, &cfg).await?;
    if cfg.record {
        record_history(&summaries, &cfg)?;
    }

    if cfg.notify_desktop && !summaries.is_empty() {
        let winner = summaries.iter().filter(|s| s.successful()).max_by(|a, b| {
            a.goodput_tokens_per_sec()
                .total_cmp(&b.goodput_tokens_per_sec())
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
        return Ok((1, summaries));
    }

    if !ttfb_gate(&summaries, &cfg) {
        return Ok((1, summaries));
    }
    let regression = loaded?;
    if !regression_gate(regression.as_ref(), &cfg) {
        return Ok((1, summaries));
    }
    Ok((0, summaries))
}
