//! Lane spawning and collection: shared by the race and ramp paths.

use super::config::{resolve_model, RunConfig};
use anyhow::Result;
use llmrace::bench::BenchConfig;
use llmrace::bench::LaneSummary;
use llmrace::tui;
use std::io::{IsTerminal as _, Write as _};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Installs a single Ctrl-C listener for a top-level command and aborts it on
/// drop so phased runs (`--ramp`, `--suite`) never leak per-phase signal tasks.
///
/// The first `SIGINT` sets the shared stop flag so in-flight rounds finish
/// cleanly; a second `SIGINT` flushes stdout/stderr and exits with code 130.
pub(crate) struct SignalGuard {
    stop: Arc<AtomicBool>,
    task: tokio::task::JoinHandle<()>,
}

impl SignalGuard {
    /// Installs a new `SignalGuard` with a fresh stop flag.
    pub(crate) fn install(message: &'static str) -> Self {
        Self::with_flag(Arc::new(AtomicBool::new(false)), message)
    }

    /// Installs a `SignalGuard` sharing an existing stop flag.
    pub(crate) fn with_flag(stop: Arc<AtomicBool>, message: &'static str) -> Self {
        let flag = Arc::clone(&stop);
        let task = tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                flag.store(true, Ordering::Relaxed);
                eprintln!("\n{message}");
                if tokio::signal::ctrl_c().await.is_ok() {
                    eprintln!("\n\u{23f9} second interrupt received — aborting immediately");
                    let _ = std::io::stdout().flush();
                    let _ = std::io::stderr().flush();
                    std::process::exit(130);
                }
            }
        });
        Self { stop, task }
    }

    /// Returns a clone of the shared stop flag.
    pub(crate) fn flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.stop)
    }
}

impl Drop for SignalGuard {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Spawns every lane (variants × providers) and collects the summaries.
///
/// `bench_override` replaces the base `BenchConfig` (used by `--ramp` to
/// race the same lanes at several concurrency levels); `headless_progress`
/// prints per-lane progress lines instead of spawning a TUI (ramp phases).
/// Signal handling is owned by the top-level caller via [`SignalGuard`].
pub(super) async fn race_collect(
    cfg: &RunConfig,
    bench_override: Option<BenchConfig>,
    headless_progress: bool,
    stop: Arc<AtomicBool>,
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
            let lane_name = p.id.to_string();
            let lane_model = model.clone();
            let lane_variant = variant.as_ref().map(|v| v.name.clone());
            let (handle, rx) = llmrace::bench::spawn_lane_stoppable(
                cfg.client.clone(),
                p.clone(),
                model,
                BenchConfig {
                    prompt: prompt.clone(),
                    variant: variant.as_ref().map(|v| Arc::from(v.name.as_str())),
                    ..bench.clone()
                },
                Arc::clone(&stop),
            );
            handles.push((lane_name, lane_model, lane_variant, async move {
                handle.await.map_err(anyhow::Error::from)
            }));
            receivers.push(rx);
        }
    }

    if headless_progress || cfg.no_tui || !std::io::stdout().is_terminal() {
        if !cfg.no_tui && !headless_progress {
            eprintln!("note: stdout is not a terminal — running headless (pass --no-tui to silence this note)");
        }
        drop(receivers);
        let mut all = Vec::new();
        for (name, model, variant, h) in handles {
            match h.await {
                Ok(s) => {
                    if headless_progress && !cfg.quiet {
                        print_lane_progress(&s, cfg);
                    }
                    all.push(s);
                }
                Err(e) => {
                    eprintln!("lane failed: {e}");
                    all.push(failed_lane_summary(name, model, variant, e));
                }
            }
        }
        Ok(all)
    } else {
        let rounds_label = match bench.duration_secs {
            Some(secs) => format!("\u{2264}{secs}s"),
            None => format!("{} rounds", bench.runs),
        };
        collect_with_tui(lanes, handles, receivers, &cfg.prompt, &rounds_label, stop).await
    }
}

fn failed_lane_summary(
    name: String,
    model: String,
    variant: Option<String>,
    err: anyhow::Error,
) -> LaneSummary {
    LaneSummary {
        name,
        model,
        variant,
        error: Some(format!("lane task failed: {err}")),
        ..LaneSummary::default()
    }
}

/// Collects lane results while the live TUI renders.
pub(super) async fn collect_with_tui(
    lanes: Vec<(Arc<str>, Option<Arc<str>>, String)>,
    handles: Vec<(
        String,
        String,
        Option<String>,
        impl std::future::Future<Output = std::result::Result<LaneSummary, anyhow::Error>>,
    )>,
    receivers: Vec<std::sync::mpsc::Receiver<llmrace::bench::LaneUpdate>>,
    prompt: &str,
    rounds_label: &str,
    stop: Arc<AtomicBool>,
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
    let tui_stop = Arc::clone(&stop);
    let tui_handle = tokio::task::spawn_blocking(move || match tui.run(rx_final) {
        Ok(finished) => {
            if !finished.is_all_done() {
                tui_stop.store(true, Ordering::Relaxed);
            }
            Ok(())
        }
        Err(e) => {
            tui_stop.store(true, Ordering::Relaxed);
            Err(e)
        }
    });

    let mut all = Vec::new();
    for (name, model, variant, h) in handles {
        match h.await {
            Ok(s) => all.push(s),
            Err(e) => {
                eprintln!("lane failed: {e}");
                all.push(failed_lane_summary(name, model, variant, e));
            }
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
