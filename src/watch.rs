//! Continuous monitoring: re-run the race on an interval and alert on
//! degradations against a rolling baseline.

use crate::bench::{spawn_lane, LaneSummary};
use crate::provider::ProviderSpec;
use crate::stats::Sample;
use std::collections::BTreeMap;
use std::time::Duration;

/// Tuning for [`watch`].
#[derive(Debug, Clone)]
pub struct WatchConfig {
    /// Seconds between race rounds.
    pub every_secs: u64,
    /// Degradation threshold in percent versus the rolling median.
    pub max_regression_pct: f64,
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
    /// All summaries, one inner vector per round.
    pub rounds: Vec<Vec<LaneSummary>>,
    /// Alerts in firing order.
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
    let limit = config.rounds.unwrap_or(usize::MAX);
    let mut round = 0usize;

    while round < limit {
        if stop() {
            break;
        }
        round += 1;
        let mut handles = Vec::new();
        for spec in &specs {
            let (handle, _progress) = spawn_lane(
                client.clone(),
                spec.clone(),
                model.clone(),
                config.bench.clone(),
            );
            handles.push(handle);
        }
        let mut summaries = Vec::new();
        for h in handles {
            if let Ok(s) = h.await {
                summaries.push(s);
            }
        }

        let mut alerts = Vec::new();
        for s in &summaries {
            if !s.successful() {
                continue;
            }
            let ttfb_hist = history.get(&s.name).cloned().unwrap_or_default();
            if let Some(alert) = degrade_check(
                round,
                &s.name,
                "ttfb",
                &ttfb_hist,
                s.mean_ttfb_ms(),
                config.max_regression_pct,
            ) {
                alerts.push(alert);
            }
            let p95_hist = p95_history.get(&s.name).cloned().unwrap_or_default();
            if let Some(alert) = degrade_check(
                round,
                &s.name,
                "p95",
                &p95_hist,
                s.p95_total_ms(),
                config.max_regression_pct,
            ) {
                alerts.push(alert);
            }
            history
                .entry(s.name.clone())
                .or_default()
                .push(s.mean_ttfb_ms());
            p95_history
                .entry(s.name.clone())
                .or_default()
                .push(s.p95_total_ms());
        }

        on_round(round, &summaries, &alerts);
        report.alerts.extend(alerts);
        report.rounds.push(summaries);

        if round < limit && !stop() {
            tokio::time::sleep(Duration::from_secs(config.every_secs)).await;
        }
    }
    report
}
