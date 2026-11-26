//! Continuous monitoring mode.

use super::config::{resolve_model, RunConfig};
use anyhow::Result;
use llmrace::report;

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

    println!(
        "\u{1f440} llmrace watch — every {}s, alert threshold {}% (Ctrl+C to stop)\n",
        cfg.every_secs, cfg.max_regression
    );

    let ctrl_c_flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_flag = std::sync::Arc::clone(&ctrl_c_flag);
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        stop_flag.store(true, std::sync::atomic::Ordering::Relaxed);
        println!("\n\u{23f9} interrupted — finishing current round, then writing reports…");
    });

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
    let record_config = cfg.clone();
    let record_failed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let failed_in_callback = record_failed.clone();
    let failed_to_stop = record_failed.clone();
    let report = llmrace::watch::watch_with_models(
        watch_lanes,
        watch_config,
        cfg.client.clone(),
        move |round, summaries, alerts| {
            if record_config.record {
                if let Err(error) = super::reports::record_history(summaries, &record_config) {
                    eprintln!("history write failed: {error}");
                    failed_in_callback.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            }
            for s in summaries {
                if s.successful() {
                    println!(
                        "  r{round} \u{2713} {:<14} {:>6.1} tok/s   TTFB {:>5.0} ms",
                        s.name,
                        s.mean_tokens_per_sec(),
                        s.mean_ttfb_ms()
                    );
                } else {
                    println!(
                        "  r{round} \u{2717} {:<14} {}",
                        s.name,
                        s.error.clone().unwrap_or_default()
                    );
                }
            }
            for a in alerts {
                println!(
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
                );
                // fire-and-forget webhook (async, can't block the sync closure)
                if let Some(url) = webhook_url.clone() {
                    let alert = a.clone();
                    let client = webhook_client.clone();
                    tokio::spawn(async move {
                        if let Err(e) = llmrace::notify::post_webhook(&client, &url, &alert).await {
                            eprintln!("  \u{26a0} alert webhook failed: {e}");
                        }
                    });
                }
                if notify_desktop {
                    llmrace::notify::desktop_notification(
                        "llmrace alert",
                        &format!("{} {} degraded +{:.0}%", a.provider, a.metric, a.delta_pct),
                    );
                }
            }
        },
        move || {
            ctrl_c_flag.load(std::sync::atomic::Ordering::Relaxed)
                || failed_to_stop.load(std::sync::atomic::Ordering::Relaxed)
        },
    )
    .await;

    println!(
        "\nsession: {} rounds, {} alert(s)",
        report.rounds_completed, report.alerts_emitted
    );
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
        let table = report::terminal_table(last);
        println!("\n{table}");
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
    Ok(i32::from(
        record_failed.load(std::sync::atomic::Ordering::Relaxed),
    ))
}
