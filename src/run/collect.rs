//! Lane spawning and collection: shared by the race and ramp paths.

use super::config::{resolve_model, RunConfig};
use anyhow::Result;
use llmrace::bench::BenchConfig;
use llmrace::bench::LaneSummary;
use llmrace::tui;
use std::io::IsTerminal as _;
use std::sync::Arc;

/// Spawns every lane (variants × providers) and collects the summaries.
///
/// `bench_override` replaces the base `BenchConfig` (used by `--ramp` to
/// race the same lanes at several concurrency levels); `headless_progress`
/// prints per-lane progress lines instead of spawning a TUI (ramp phases).
pub(super) async fn race_collect(
    cfg: &RunConfig,
    bench_override: Option<BenchConfig>,
    headless_progress: bool,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Result<Vec<LaneSummary>> {
    let bench = bench_override.unwrap_or_else(|| cfg.bench.clone());
    let variants: Vec<Option<llmrace::variants::VariantDef>> = match &cfg.sweep {
        Some(spec) => llmrace::variants::parse_with_custom(spec, &cfg.custom_variants)
            .map(|vs| vs.into_iter().map(Some).collect())
            .map_err(|e| anyhow::anyhow!("{e}"))?,
        None => vec![None],
    };

    let mut lanes: Vec<(Arc<str>, Option<Arc<str>>, String)> = Vec::new();
    let mut handles = Vec::new();
    let mut receivers = Vec::new();
    for variant in &variants {
        for p in &cfg.providers {
            lanes.push((
                Arc::clone(&p.id),
                variant.as_ref().map(|v| Arc::from(v.name.as_str())),
                resolve_model(&cfg.model_overrides, &p.id, &p.default_model),
            ));
        }
        let prompt = match variant {
            Some(v) => v.prompt.to_string(),
            None => bench.prompt.clone(),
        };
        for p in &cfg.providers {
            let model = resolve_model(&cfg.model_overrides, &p.id, &p.default_model);
            let (handle, rx) = llmrace::bench::spawn_lane_stoppable(
                cfg.client.clone(),
                p.clone(),
                model,
                BenchConfig {
                    prompt: prompt.clone(),
                    variant: variant.as_ref().map(|v| Arc::from(v.name.as_str())),
                    ..bench.clone()
                },
                std::sync::Arc::clone(&stop),
            );
            handles.push(async move { handle.await.map_err(anyhow::Error::from) });
            receivers.push(rx);
        }
    }

    if headless_progress || cfg.no_tui || !std::io::stdout().is_terminal() {
        if !cfg.no_tui && !headless_progress {
            eprintln!("note: stdout is not a terminal — running headless (pass --no-tui to silence this note)");
        }
        // graceful interrupt: SIGINT stops new rounds; in-flight rounds
        // finish and partial results still reach the reports
        {
            let stop = std::sync::Arc::clone(&stop);
            tokio::spawn(async move {
                let _ = tokio::signal::ctrl_c().await;
                stop.store(true, std::sync::atomic::Ordering::Relaxed);
                eprintln!("\n\u{23f9} interrupt received — finishing in-flight rounds, then writing reports…");
            });
        }
        drop(receivers);
        let mut all = Vec::new();
        for h in handles {
            match h.await {
                Ok(s) => {
                    if headless_progress && !cfg.quiet {
                        print_lane_progress(&s, cfg);
                    }
                    all.push(s);
                }
                Err(e) => eprintln!("lane failed: {e}"),
            }
        }
        Ok(all)
    } else {
        let rounds_label = match bench.duration_secs {
            Some(secs) => format!("\u{2264}{secs}s"),
            None => format!("{} rounds", bench.runs),
        };
        collect_with_tui(lanes, handles, receivers, &cfg.prompt, &rounds_label).await
    }
}

/// Collects lane results while the live TUI renders.
pub(super) async fn collect_with_tui(
    lanes: Vec<(Arc<str>, Option<Arc<str>>, String)>,
    handles: Vec<
        impl std::future::Future<Output = std::result::Result<LaneSummary, anyhow::Error>>,
    >,
    receivers: Vec<std::sync::mpsc::Receiver<llmrace::bench::LaneUpdate>>,
    prompt: &str,
    rounds_label: &str,
) -> Result<Vec<LaneSummary>> {
    let merged = std::sync::mpsc::channel::<llmrace::bench::LaneUpdate>();
    for rx in receivers {
        let tx = merged.0.clone();
        tokio::task::spawn_blocking(move || {
            while let Ok(u) = rx.recv() {
                if tx.send(u).is_err() {
                    break;
                }
            }
        });
    }
    drop(merged.0);
    let tui = tui::Tui::with_rounds_label(lanes, prompt.to_string(), rounds_label.to_string());
    let rx_final = merged.1;
    let tui_handle = tokio::task::spawn_blocking(move || tui.run(rx_final).map(|_| ()));

    let mut all = Vec::new();
    for h in handles {
        match h.await {
            Ok(s) => all.push(s),
            Err(e) => eprintln!("lane failed: {e}"),
        }
    }
    match tui_handle.await {
        Ok(Ok(())) => Ok(all),
        Ok(Err(e)) => Err(e),
        Err(e) => Err(e.into()),
    }
}

pub(super) fn print_lane_progress(s: &LaneSummary, cfg: &RunConfig) {
    if let Some(e) = &s.error {
        cfg.status(format!("  \u{2717} {:<14} {}", s.name, e));
    } else {
        let ci = match s.ci95_total_ms() {
            Some((lo, hi)) => format!("\u{b1}{:.0}", (hi - lo) / 2.0),
            None => "—".to_string(),
        };
        cfg.status(format!(
            "  \u{2713} {:<14} {:>6.1} tok/s   {:>6.0} ms {}   TTFB {:>4} ms",
            s.name,
            s.mean_tokens_per_sec(),
            s.mean_total_ms(),
            ci,
            llmrace::stats::fmt_ms(s.mean_ttfb_ms())
        ));
    }
}
