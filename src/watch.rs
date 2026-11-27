//! Continuous monitoring: re-run the race on an interval and alert on
//! degradations against a rolling baseline.

use crate::bench::LaneSummary;
use crate::provider::ProviderSpec;
use crate::stats::Sample;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Tuning for [`watch`].
#[derive(Debug, Clone)]
pub struct WatchConfig {
    /// Seconds between race rounds.
    pub every_secs: u64,
    /// Degradation threshold in percent versus the rolling median.
    pub max_regression_pct: f64,
    /// Consecutive degraded rounds required before an alert fires
    /// (debounce; 1 keeps the immediate behavior).
    pub alert_rounds: usize,
    /// Total rounds to run; `None` until interrupted.
    pub rounds: Option<usize>,
    /// Bench configuration forwarded to every lane.
    pub bench: crate::bench::BenchConfig,
}

/// One detected degradation.
#[derive(Debug, Clone, PartialEq)]
pub struct Alert {
    /// Round index (1-based) where the alert fired.
    pub round: usize,
    /// Provider that degraded.
    pub provider: String,
    /// Metric that degraded.
    pub metric: &'static str,
    /// Rolling median baseline value.
    pub baseline: f64,
    /// Observed value in this round.
    pub current: f64,
    /// Degradation versus the rolling median, in percent.
    pub delta_pct: f64,
}

/// Result of a watch session.
#[derive(Debug, Clone, Default)]
pub struct WatchReport {
    /// Number of rounds completed, including rounds no longer retained.
    pub rounds_completed: usize,
    /// Total alerts emitted, including alerts no longer retained.
    pub alerts_emitted: usize,
    /// All summaries for finite sessions; the latest round for unbounded
    /// monitoring. Use the callback to persist every round.
    pub rounds: Vec<Vec<LaneSummary>>,
    /// Alerts in firing order (latest 128 for unbounded monitoring).
    pub alerts: Vec<Alert>,
}

impl WatchReport {
    /// Summaries of the most recent round, or an empty slice.
    pub fn last_round(&self) -> &[LaneSummary] {
        self.rounds.last().map(|v| v.as_slice()).unwrap_or(&[])
    }
}

/// Compares a fresh observation against the rolling history.
///
/// Fires when the current value exceeds the median of the (non-empty)
/// history by more than `threshold_pct` percent.
pub fn degrade_check(
    round: usize,
    provider: &str,
    metric: &'static str,
    history: &[f64],
    current: f64,
    threshold_pct: f64,
) -> Option<Alert> {
    if history.is_empty() {
        return None;
    }
    let baseline = Sample::from_values(history.to_vec()).median();
    if baseline.abs() < f64::EPSILON {
        return None;
    }
    let delta_pct = (current - baseline) / baseline * 100.0;
    if delta_pct > threshold_pct {
        Some(Alert {
            round,
            provider: provider.to_string(),
            metric,
            baseline,
            current,
            delta_pct,
        })
    } else {
        None
    }
}

/// Tracks the consecutive-degradation streak for one (provider, metric);
/// returns the streak after this round's update.
fn streak_of(
    streaks: &mut BTreeMap<(String, &'static str), usize>,
    provider: &str,
    metric: &'static str,
    degraded_this_round: bool,
) -> usize {
    let key = (provider.to_string(), metric);
    let entry = streaks.entry(key).or_default();
    if degraded_this_round {
        *entry += 1;
    } else {
        *entry = 0;
    }
    *entry
}

/// Test-visible wrapper for [`streak_of`].
#[doc(hidden)]
pub fn streak_of_pub(
    streaks: &mut BTreeMap<(String, &'static str), usize>,
    provider: &str,
    metric: &'static str,
    degraded_this_round: bool,
) -> usize {
    streak_of(streaks, provider, metric, degraded_this_round)
}

/// Runs watch rounds until interrupted or the round limit is reached.
///
/// `stop` is polled between rounds and ends the loop gracefully. Every round
/// races all providers concurrently; each lane's TTFB and p95 are compared
/// against the median of previous rounds and alerts are recorded.
pub async fn watch<F, F2>(
    specs: Vec<ProviderSpec>,
    model: String,
    config: WatchConfig,
    client: reqwest::Client,
    on_round: F,
    stop: F2,
) -> WatchReport
where
    F: FnMut(usize, &[LaneSummary], &[Alert]) + Send + 'static,
    F2: FnMut() -> bool + Send + 'static,
{
    let lanes: Vec<(ProviderSpec, String)> = specs
        .into_iter()
        .map(|spec| {
            let model = model.clone();
            (spec, model)
        })
        .collect();
    watch_with_models(lanes, config, client, on_round, stop).await
}

/// Like [`watch`], but each provider races its own model — required for
/// `provider=model` lanes and per-provider `-m` overrides.
pub async fn watch_with_models<F, F2>(
    lanes: Vec<(ProviderSpec, String)>,
    config: WatchConfig,
    client: reqwest::Client,
    mut on_round: F,
    mut stop: F2,
) -> WatchReport
where
    F: FnMut(usize, &[LaneSummary], &[Alert]) + Send + 'static,
    F2: FnMut() -> bool + Send + 'static,
{
    let mut report = WatchReport::default();
    let mut history: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    let mut p95_history: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    let mut streaks: BTreeMap<(String, &'static str), usize> = BTreeMap::new();
    let limit = config.rounds.unwrap_or(usize::MAX);
    let mut round = 0usize;
    let lane_stop = Arc::new(AtomicBool::new(false));

    while round < limit {
        if lane_stop.load(Ordering::Relaxed) || stop() {
            lane_stop.store(true, Ordering::Relaxed);
            break;
        }
        round += 1;
        let mut handles: Vec<(String, String, tokio::task::JoinHandle<LaneSummary>)> =
            Vec::with_capacity(lanes.len());
        for (spec, model) in &lanes {
            let handle = crate::bench::engine::spawn_lane_with_sender(
                client.clone(),
                spec.clone(),
                model.clone(),
                config.bench.clone(),
                Arc::clone(&lane_stop),
                None,
            );
            handles.push((spec.id.to_string(), model.clone(), handle));
        }
        let mut summaries = Vec::with_capacity(handles.len());
        let poll_step = Duration::from_millis(100);
        let mut poll_tick =
            tokio::time::interval_at(tokio::time::Instant::now() + poll_step, poll_step);
        poll_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        for (name, model, mut handle) in handles {
            let res = loop {
                tokio::select! {
                    res = &mut handle => break res,
                    _ = poll_tick.tick(), if !lane_stop.load(Ordering::Relaxed) => {
                        if stop() {
                            lane_stop.store(true, Ordering::Relaxed);
                        }
                    }
                }
            };
            match res {
                Ok(s) => summaries.push(s),
                Err(e) => summaries.push(LaneSummary {
                    name,
                    model,
                    error: Some(format!("lane task failed: {e}")),
                    prompt: config.bench.prompt.clone(),
                    concurrency: config.bench.concurrency,
                    variant: config.bench.variant.as_ref().map(|v| v.to_string()),
                    tool_task: Some(config.bench.tool_task.clone()),
                    price_book: config.bench.price_book.clone(),
                    ..LaneSummary::default()
                }),
            }
        }

        let mut alerts = Vec::new();
        for s in &summaries {
            let key = format!("{}/{}", s.name, s.model);
            let failed = !s.successful() || s.failed_rounds() > 0;
            let streak = streak_of(&mut streaks, &key, "availability", failed);
            if streak == config.alert_rounds.max(1) && failed {
                let rate = s.failure_rate().unwrap_or(1.0) * 100.0;
                alerts.push(Alert {
                    round,
                    provider: key.clone(),
                    metric: "availability",
                    baseline: 0.0,
                    current: rate,
                    delta_pct: rate,
                });
            }
            if !s.successful() {
                streak_of(&mut streaks, &key, "ttfb", false);
                streak_of(&mut streaks, &key, "p95", false);
                continue;
            }
            let ttfb_hist = history.get(&key).map(Vec::as_slice).unwrap_or(&[]);
            let ttfb_degraded = degrade_check(
                round,
                &key,
                "ttfb",
                ttfb_hist,
                s.mean_ttfb_ms(),
                config.max_regression_pct,
            );
            let ttfb_streak = streak_of(&mut streaks, &key, "ttfb", ttfb_degraded.is_some());
            if ttfb_streak == config.alert_rounds.max(1) {
                if let Some(alert) = ttfb_degraded {
                    alerts.push(alert);
                }
            }
            let p95_hist = p95_history.get(&key).map(Vec::as_slice).unwrap_or(&[]);
            let p95_degraded = degrade_check(
                round,
                &key,
                "p95",
                p95_hist,
                s.p95_total_ms(),
                config.max_regression_pct,
            );
            let p95_streak = streak_of(&mut streaks, &key, "p95", p95_degraded.is_some());
            if p95_streak == config.alert_rounds.max(1) {
                if let Some(alert) = p95_degraded {
                    alerts.push(alert);
                }
            }
            push_window(history.entry(key.clone()).or_default(), s.mean_ttfb_ms());
            push_window(p95_history.entry(key).or_default(), s.p95_total_ms());
        }

        on_round(round, &summaries, &alerts);
        report.rounds_completed = round;
        report.alerts_emitted += alerts.len();
        report.alerts.extend(alerts);
        if config.rounds.is_none() {
            report.rounds.clear();
            if report.alerts.len() > 128 {
                report.alerts.drain(..report.alerts.len() - 128);
            }
        }
        report.rounds.push(summaries);

        if round < limit && !lane_stop.load(Ordering::Relaxed) && !stop() {
            let started = std::time::Instant::now();
            let interval = Duration::from_secs(config.every_secs);
            while started.elapsed() < interval {
                if lane_stop.load(Ordering::Relaxed) || stop() {
                    lane_stop.store(true, Ordering::Relaxed);
                    break;
                }
                tokio::time::sleep(
                    interval
                        .saturating_sub(started.elapsed())
                        .min(Duration::from_millis(100)),
                )
                .await;
            }
        }
    }
    report
}

fn push_window(values: &mut Vec<f64>, value: f64) {
    if values.len() == 128 {
        values.remove(0);
    }
    values.push(value);
}
