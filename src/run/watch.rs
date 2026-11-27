//! Continuous monitoring mode.

use super::collect::SignalGuard;
use super::config::{resolve_model, RunConfig};
use anyhow::Result;
use llmrace::report;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const WEBHOOK_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

/// `--watch`: re-runs the race on an interval, alerting on degradation,
/// and returns the process exit code.
pub(crate) async fn watch_race(cfg: RunConfig) -> Result<i32> {
    let watch_config = llmrace::watch::WatchConfig {
        every_secs: cfg.every_secs,
        max_regression_pct: cfg.max_regression,
        alert_rounds: cfg.alert_rounds,
        rounds: None,
        bench: cfg.bench.clone(),
    };

    if !cfg.quiet {
        cfg.status(format!(
            "\u{1f440} llmrace watch — every {}s, alert threshold {}% (Ctrl+C to stop)\n",
            cfg.every_secs, cfg.max_regression
        ));
    }

    let signal_guard = SignalGuard::install(
        "\u{23f9} interrupted — finishing current round, then writing reports…",
    );
    let ctrl_c_flag = signal_guard.flag();
    let stop_check = Arc::clone(&ctrl_c_flag);

    let watch_lanes: Vec<(llmrace::provider::ProviderSpec, String)> = cfg
        .providers
        .iter()
        .map(|p| {
            (
                p.clone(),
                resolve_model(&cfg.model_overrides, &p.id, &p.default_model),
            )
        })
        .collect();
    let webhook_url = cfg.alert_webhook.clone();
    let notify_desktop = cfg.notify_desktop;
    let webhook_client = cfg.client.clone();
    let status_cfg = cfg.clone();
    let record_failed = Arc::new(AtomicBool::new(false));
    let failed_in_callback = Arc::clone(&record_failed);
    let failed_to_stop = Arc::clone(&record_failed);
    let record_handles: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>> =
        Arc::new(Mutex::new(Vec::new()));
    let record_handles_cb = Arc::clone(&record_handles);
    let webhook_handles: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>> =
        Arc::new(Mutex::new(Vec::new()));
    let webhook_handles_cb = Arc::clone(&webhook_handles);

    let report = llmrace::watch::watch_with_models(
        watch_lanes,
        watch_config,
        cfg.client.clone(),
        move |round, summaries, alerts| {
            if status_cfg.record {
                let summaries_owned = summaries.to_vec();
                let record_cfg = status_cfg.clone();
                let failed = Arc::clone(&failed_in_callback);
                let handle = tokio::task::spawn_blocking(move || {
                    if let Err(error) =
                        super::reports::record_history(&summaries_owned, &record_cfg)
                    {
                        eprintln!("history write failed: {error}");
                        failed.store(true, Ordering::Relaxed);
                    }
                });
                let mut guard = record_handles_cb
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                guard.retain(|h| !h.is_finished());
                guard.push(handle);
            }
            if !status_cfg.quiet {
                for s in summaries {
                    if s.successful() {
                        status_cfg.status(format!(
                            "  r{round} \u{2713} {:<14} {:>6.1} tok/s   TTFB {:>5.0} ms",
                            s.name,
                            s.mean_tokens_per_sec(),
                            s.mean_ttfb_ms()
                        ));
                    } else {
                        status_cfg.status(format!(
                            "  r{round} \u{2717} {:<14} {}",
                            s.name,
                            s.error.clone().unwrap_or_default()
                        ));
                    }
                }
            }
            for a in alerts {
                if !status_cfg.quiet {
                    status_cfg.status(format!(
                        "  \u{26a0} ALERT r{} {} {} {:.0} \u{2192} {:.0} {} (+{:.0}%)",
                        a.round,
                        a.provider,
                        a.metric,
                        a.baseline,
                        a.current,
                        if a.metric == "availability" {
                            "% failed rounds"
                        } else {
                            "ms"
                        },
                        a.delta_pct
                    ));
                }
                if let Some(url) = webhook_url.clone() {
                    let alert = a.clone();
                    let client = webhook_client.clone();
                    let handle = tokio::spawn(async move {
                        if let Err(e) = llmrace::notify::post_webhook(&client, &url, &alert).await {
                            eprintln!("  \u{26a0} alert webhook failed: {e}");
                        }
                    });
                    let mut guard = webhook_handles_cb
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    guard.retain(|h| !h.is_finished());
                    guard.push(handle);
                }
                if notify_desktop {
                    llmrace::notify::desktop_notification(
                        "llmrace alert",
                        &format!("{} {} degraded +{:.0}%", a.provider, a.metric, a.delta_pct),
                    );
                }
            }
        },
        move || stop_check.load(Ordering::Relaxed) || failed_to_stop.load(Ordering::Relaxed),
    )
    .await;
    drop(signal_guard);

    // Ensure every offloaded history write completes before reporting or exiting.
    let pending_records: Vec<tokio::task::JoinHandle<()>> = {
        let mut guard = record_handles
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        std::mem::take(&mut *guard)
    };
    for handle in pending_records {
        let _ = handle.await;
    }

    // Drain in-flight alert webhooks briefly so final alerts are not dropped on exit.
    let pending_webhooks: Vec<tokio::task::JoinHandle<()>> = {
        let mut guard = webhook_handles
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        std::mem::take(&mut *guard)
    };
    if !pending_webhooks.is_empty() {
        let _ = tokio::time::timeout(WEBHOOK_DRAIN_TIMEOUT, async {
            for handle in pending_webhooks {
                let _ = handle.await;
            }
        })
        .await;
    }

    if !cfg.quiet {
        if ctrl_c_flag.load(Ordering::Relaxed) {
            cfg.status("\n\u{23f9} interrupted — writing reports");
        }
        cfg.status(format!(
            "\nsession: {} rounds, {} alert(s)",
            report.rounds_completed, report.alerts_emitted
        ));
    }
    if cfg.notify_desktop {
        llmrace::notify::desktop_notification(
            "llmrace watch complete",
            &format!(
                "{} rounds, {} alert(s) — report: {}",
                report.rounds_completed, report.alerts_emitted, cfg.out
            ),
        );
    }

    let last = report.last_round();
    if !last.is_empty() {
        if !cfg.quiet {
            let table = report::terminal_table(last);
            cfg.status(format!("\n{table}"));
        }
        let regression = super::reports::load_regression(last, &cfg)?;
        super::reports::write_reports(last, &cfg, regression.as_ref())?;
        super::reports::export_metrics(last, &cfg).await?;
        if llmrace::gates::all_lanes_failed(last)
            || !super::reports::ttfb_gate(last, &cfg)
            || !super::reports::regression_gate(regression.as_ref(), &cfg)
        {
            return Ok(1);
        }
    }
    Ok(i32::from(record_failed.load(Ordering::Relaxed)))
}
