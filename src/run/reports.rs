//! Report writing, metric export, history recording and CI gates.

use super::config::RunConfig;
use anyhow::{Context, Result};
use llmrace::bench::LaneSummary;
use llmrace::report;

/// Writes the markdown/JSON/CSV/HTML reports requested by the config.
pub(super) fn write_reports(summaries: &[LaneSummary], cfg: &RunConfig) -> Result<()> {
    // Report the rounds actually measured: `--duration` uses a sentinel
    // round count internally and a graceful interrupt may stop early —
    // neither belongs in report metadata.
    let actual_runs = summaries
        .iter()
        .map(|s| s.runs.len())
        .max()
        .unwrap_or(cfg.bench.runs);
    let mut md = report::markdown(summaries, &cfg.prompt, actual_runs, cfg.bench.max_tokens);
    if let Some(note) = report::sampling_note(&cfg.bench.sampling) {
        if let Some(pos) = md.find("- **Generated**:") {
            md.insert_str(pos, &note);
        } else {
            md.push_str(&note);
        }
    }
    if cfg.winner {
        let scored = llmrace::score::score_lanes(summaries, &cfg.weights);
        let block = llmrace::score::podium(&scored, &cfg.weights);
        if !block.is_empty() {
            md.push_str("\n## Podium\n");
            md.push_str(&block);
        }
    }
    if cfg.histogram {
        md.push_str(&report::histogram_section(summaries));
    }
    if cfg.out == "-" {
        // markdown to stdout (piping into CI summaries); sibling formats
        // fall back to the conventional filenames
        println!("{md}");
    } else {
        std::fs::write(&cfg.out, &md).with_context(|| format!("writing {}", cfg.out))?;
        println!("\u{1f4c4} markdown report saved to {}", cfg.out);
    }

    let sibling = |ext: &str| -> String {
        let base = if cfg.out == "-" {
            std::path::Path::new("llmrace-report")
        } else {
            std::path::Path::new(&cfg.out)
        };
        if base.extension().is_some() {
            base.with_extension(ext).to_string_lossy().into_owned()
        } else {
            format!("{}.{ext}", base.display())
        }
    };
    if cfg.json {
        let j = report::insert_sampling_json(
            &report::json_report_with_region(
                summaries,
                &cfg.prompt,
                actual_runs,
                cfg.bench.max_tokens,
                cfg.region.as_deref(),
            ),
            &cfg.bench.sampling,
        );
        let path = sibling("json");
        std::fs::write(&path, j).with_context(|| format!("writing {path}"))?;
        println!("\u{1f4ca} json report saved to {path}");
    }
    if cfg.csv {
        let c = report::csv(summaries);
        let path = sibling("csv");
        std::fs::write(&path, c).with_context(|| format!("writing {path}"))?;
        println!("\u{1f4c8} csv report saved to {path}");
    }
    if cfg.html {
        let scored = if cfg.winner {
            llmrace::score::score_lanes(summaries, &cfg.weights)
        } else {
            Vec::new()
        };
        let mut h = report::html_report(
            summaries,
            &cfg.prompt,
            cfg.bench.runs,
            cfg.bench.max_tokens,
            &scored,
        );
        if let Some(inline) = report::sampling_inline(&cfg.bench.sampling) {
            let escaped = llmrace::util::html_escape(&inline);
            h = h.replacen("</code> ·", &format!("</code> · {escaped} ·"), 1);
        }
        let path = sibling("html");
        std::fs::write(&path, h).with_context(|| format!("writing {path}"))?;
        println!("\u{1f310} html report saved to {path}");
    }
    Ok(())
}

/// Pushes metrics to the Prometheus pushgateway and/or writes them to a
/// file, per configuration.
pub(super) async fn export_metrics(summaries: &[LaneSummary], cfg: &RunConfig) -> Result<()> {
    if let Some(gateway) = &cfg.push_gateway {
        llmrace::prom::push(&cfg.client, gateway, summaries)
            .await
            .with_context(|| format!("pushing metrics to {gateway}"))?;
        println!("\u{1f4e1} metrics pushed to {gateway}");
    }
    if let Some(path) = &cfg.prom_output {
        let body = llmrace::prom::render_metrics(summaries);
        std::fs::write(path, body)
            .with_context(|| format!("writing Prometheus metrics to {path}"))?;
        println!("\u{1f4e1} Prometheus metrics saved to {path}");
    }
    Ok(())
}

/// Appends lanes to the persistent history (jsonl or sqlite backend).
pub(super) fn record_history(summaries: &[LaneSummary], storage: &str) -> Result<()> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let records: Vec<llmrace::history::HistoryRecord> = summaries
        .iter()
        .filter(|s| s.successful())
        .map(|s| llmrace::history::record_from_summary(s, now))
        .collect();
    match storage {
        "jsonl" => {
            let path = llmrace::history::history_path();
            llmrace::history::append(&path, &records)
                .with_context(|| format!("recording history to {}", path.display()))?;
            println!(
                "\u{1f5c2} recorded {} lane(s) to {}",
                records.len(),
                path.display()
            );
        }
        #[cfg(feature = "sqlite")]
        "sqlite" => {
            let path = crate::sqlite_path();
            let conn = llmrace::sqlite::open(&path).with_context(|| "opening sqlite history")?;
            llmrace::sqlite::save(&conn, &records).with_context(|| "writing sqlite history")?;
            println!(
                "\u{1f5c2} recorded {} lane(s) to {}",
                records.len(),
                path.display()
            );
        }
        other => anyhow::bail!("unknown storage backend '{other}' (expected jsonl or sqlite)"),
    }
    Ok(())
}

/// `--max-ttfb` gate: returns `false` when any provider exceeded the
/// threshold (the process should exit non-zero).
pub(super) fn ttfb_gate(summaries: &[LaneSummary], cfg: &RunConfig) -> bool {
    let Some(max_ttfb) = cfg.max_ttfb else {
        return true;
    };
    let offenders = llmrace::gates::ttfb_offenders(summaries, max_ttfb);
    let verdict = llmrace::gates::render_ttfb_verdict(&offenders, max_ttfb, cfg.quiet);
    if verdict.failed() {
        eprintln!("\n{}", verdict.message());
        return false;
    }
    if !verdict.message().is_empty() {
        println!("{}", verdict.message());
    }
    true
}

/// `--baseline` regression gate: returns `false` on regression.
pub(super) fn regression_gate(summaries: &[LaneSummary], cfg: &RunConfig) -> Result<bool> {
    let Some(baseline_path) = &cfg.baseline else {
        return Ok(true);
    };
    let raw = std::fs::read_to_string(baseline_path)
        .with_context(|| format!("reading baseline {baseline_path}"))?;
    let baseline = llmrace::regression::BaselineReport::from_json_str(&raw)
        .with_context(|| format!("parsing baseline {baseline_path}"))?;
    let regression = llmrace::regression::evaluate(summaries, &baseline, cfg.max_regression);
    println!("\n{}", regression.terminal_table(cfg.max_regression));
    if cfg.require_significance {
        if regression.any_regressed() && !regression.any_regressed_significant() {
            println!("\u{2705} threshold breached but no regression is statistically significant — treating as noise (--require-significance)");
        }
        Ok(!regression.any_regressed_significant())
    } else {
        Ok(!regression.any_regressed())
    }
}
