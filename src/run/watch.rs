//! Continuous monitoring mode.

use super::config::{resolve_model, RunConfig};
use anyhow::{Context, Result};
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
    let report = llmrace::watch::watch_with_models(
        watch_lanes,
        watch_config,
        cfg.client.clone(),
        move |round, summaries, alerts| {
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
                    "  \u{26a0} ALERT r{} {} {} {:.0} \u{2192} {:.0} ms (+{:.0}%)",
                    a.round, a.provider, a.metric, a.baseline, a.current, a.delta_pct
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
        move || ctrl_c_flag.load(std::sync::atomic::Ordering::Relaxed),
    )
    .await;

    println!(
        "\nsession: {} rounds, {} alert(s)",
        report.rounds.len(),
        report.alerts.len()
    );
    if cfg.notify_desktop {
        llmrace::notify::desktop_notification(
            "llmrace watch complete",
            &format!(
                "{} rounds, {} alert(s) — report: {}",
                report.rounds.len(),
                report.alerts.len(),
                cfg.out
            ),
        );
    }

    if cfg.record {
        let mut records = Vec::new();
        for summaries in &report.rounds {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let request = report::request_sha256(
                &cfg.bench.prompt,
                &cfg.bench.workload,
                &cfg.bench.sampling,
                cfg.bench.max_tokens,
                cfg.bench.tools.then_some(cfg.bench.tool_task.as_str()),
            );
            for s in summaries.iter().filter(|s| s.successful()) {
                let mut record = llmrace::history::record_from_summary(s, now);
                record.request_sha256 = Some(request.clone());
                records.push(record);
            }
        }
        let path = llmrace::history::history_path();
        llmrace::history::append(&path, &records)
            .with_context(|| format!("recording history to {}", path.display()))?;
        println!(
            "\u{1f5c2} recorded {} lane(s) to {}",
            records.len(),
            path.display()
        );
    }

    let last = report.last_round();
    if !last.is_empty() {
        let table = report::terminal_table(last);
        println!("\n{table}");
        let md = report::markdown(last, &cfg.prompt, cfg.bench.runs, cfg.bench.max_tokens);
        std::fs::write(&cfg.out, md).with_context(|| format!("writing {}", cfg.out))?;
        println!("\u{1f4c4} report saved to {}", cfg.out);
    }
    Ok(0)
}
