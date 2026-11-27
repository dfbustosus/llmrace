//! Provider recommendation from accumulated race history.
//!
//! `llmrace --recommend` analyzes recorded races and answers the question
//! every benchmark eventually leads to: *which provider should I use?*
//! It ranks providers on four axes (speed, throughput, stability, cost)
//! and produces a plain-language recommendation.

use crate::history::HistoryRecord;
use crate::stats::Sample;

/// One provider's accumulated profile across recorded races.
#[derive(Debug, Clone)]
pub struct ProviderProfile {
    /// Estimated monthly cost at the given daily request volume, USD.
    pub monthly_cost_usd: Option<f64>,
    /// Value for money: median tokens per second divided by cost per run in USD
    /// (`tok/s per $/run`). Higher = more output per unit cost.
    pub value_for_money: Option<f64>,
    /// Provider id.
    pub name: String,
    /// Most recent model used.
    pub current_model: String,
    /// Number of recorded races.
    pub races: usize,
    /// Median mean-latency across races, ms.
    pub median_latency_ms: f64,
    /// Median TTFB across races, ms.
    pub median_ttfb_ms: f64,
    /// Median throughput across races, tokens/s.
    pub median_tokens_per_sec: f64,
    /// Coefficient of variation of latency (stddev/mean): lower = more stable.
    pub latency_cv: f64,
    /// Estimated cost per run in USD, when pricing is known.
    pub cost_per_run_usd: Option<f64>,
    /// Median goodput across races that recorded it. `None` on history
    /// written before failed rounds were kept; ranking then uses tokens/s.
    pub median_goodput: Option<f64>,
    /// Median failure rate across races that recorded it.
    pub median_failure_rate: Option<f64>,
    /// Median task-pass rate across races that recorded one.
    pub median_task_pass: Option<f64>,
}

impl ProviderProfile {
    /// Throughput used for ranking: goodput when history has it, otherwise
    /// raw tokens/s.
    #[must_use]
    pub fn rank_throughput(&self) -> f64 {
        self.median_goodput.unwrap_or(self.median_tokens_per_sec)
    }

    /// A one-line quality summary.
    #[must_use]
    pub fn headline(&self) -> String {
        let stability = if self.latency_cv < 0.10 {
            "very stable"
        } else if self.latency_cv < 0.25 {
            "stable"
        } else {
            "variable"
        };
        format!(
            "{} ({} races, {:.0} ms median, {:.0} tok/s, {stability})",
            self.name, self.races, self.median_latency_ms, self.median_tokens_per_sec,
        )
    }
}

/// Builds provider profiles from history records.
#[must_use]
pub fn build_profiles(records: &[HistoryRecord]) -> Vec<ProviderProfile> {
    // group by provider (using the most recent model per provider)
    let mut providers: Vec<String> = Vec::new();
    for r in records {
        if !providers.contains(&r.provider) {
            providers.push(r.provider.clone());
        }
    }

    providers
        .into_iter()
        .filter_map(|name| {
            let latest = records
                .iter()
                .filter(|r| r.provider == name)
                .max_by_key(|r| r.timestamp_unix)?;
            if latest.failure_rate == Some(1.0) {
                return None;
            }
            let races: Vec<&HistoryRecord> = records
                .iter()
                .filter(|r| {
                    r.provider == name
                        && r.model == latest.model
                        && r.variant == latest.variant
                        && r.concurrency == latest.concurrency
                        && r.request_sha256 == latest.request_sha256
                })
                .collect();
            if races.is_empty() {
                return None;
            }
            let latencies: Vec<f64> = races
                .iter()
                .filter(|r| r.failure_rate != Some(1.0))
                .map(|r| r.mean_total_ms)
                .collect();
            let ttfbs: Vec<f64> = races
                .iter()
                .filter(|r| r.failure_rate != Some(1.0))
                .map(|r| r.mean_ttfb_ms)
                .collect();
            let tps: Vec<f64> = races
                .iter()
                .filter(|r| r.failure_rate != Some(1.0))
                .map(|r| r.mean_tokens_per_sec)
                .collect();
            let goodputs: Vec<f64> = races
                .iter()
                .filter_map(|r| r.goodput_tokens_per_sec)
                .collect();
            let rates: Vec<f64> = races.iter().filter_map(|r| r.failure_rate).collect();
            let tasks: Vec<f64> = races.iter().filter_map(|r| r.task_pass_rate).collect();
            let recorded_costs: Vec<f64> =
                races.iter().filter_map(|r| r.cost_per_run_usd).collect();
            let sample = Sample::from_values(latencies.clone());
            let estimated = (!tps.is_empty() && !latencies.is_empty())
                .then(|| crate::cost::pricing(&name, &latest.model))
                .flatten()
                .map(|(_pin, pout)| {
                    let avg_tokens = tps.iter().sum::<f64>() / tps.len() as f64
                        * (latencies.iter().sum::<f64>() / latencies.len() as f64 / 1000.0);
                    avg_tokens * pout / 1_000_000.0
                });
            // A recorded bill beats the token estimate. History written
            // before cost was stored still uses the estimate.
            let cost = if recorded_costs.is_empty() {
                estimated
            } else {
                Some(Sample::from_values(recorded_costs).median())
            };
            let monthly_cost = cost.map(|per_run| per_run * 1000.0 * 30.0);
            let median_tps = Sample::from_values(tps.clone()).median();
            let vfm = cost.map(|per_run| {
                if per_run > 0.0 && per_run.is_finite() {
                    median_tps / per_run
                } else {
                    0.0
                }
            });
            Some(ProviderProfile {
                monthly_cost_usd: monthly_cost,
                value_for_money: vfm,
                name,
                current_model: latest.model.clone(),
                races: races.len(),
                median_latency_ms: sample.median(),
                median_ttfb_ms: Sample::from_values(ttfbs).median(),
                median_tokens_per_sec: Sample::from_values(tps).median(),
                latency_cv: if sample.mean() > 0.0 {
                    sample.stddev() / sample.mean()
                } else {
                    0.0
                },
                cost_per_run_usd: cost,
                median_goodput: (!goodputs.is_empty())
                    .then(|| Sample::from_values(goodputs).median()),
                median_failure_rate: (!rates.is_empty())
                    .then(|| Sample::from_values(rates).median()),
                median_task_pass: (!tasks.is_empty()).then(|| Sample::from_values(tasks).median()),
            })
        })
        .collect()
}

/// Renders the recommendation report.
#[must_use]
pub fn render_recommendation(profiles: &[ProviderProfile]) -> String {
    if profiles.is_empty() {
        return "no recorded history — run `llmrace ... --record` to start recording".into();
    }

    let mut out = String::from("🧭 llmrace provider recommendation\n\n");

    // best on each axis
    if let Some(fastest) = profiles
        .iter()
        .min_by(|a, b| a.median_latency_ms.total_cmp(&b.median_latency_ms))
    {
        out.push_str(&format!(
            "  ⚡ Fastest:  {} — {:.0} ms median latency ({})\n",
            fastest.name, fastest.median_latency_ms, fastest.current_model
        ));
    }
    if let Some(throughput) = profiles
        .iter()
        .max_by(|a, b| a.rank_throughput().total_cmp(&b.rank_throughput()))
    {
        let label = if throughput.median_goodput.is_some() {
            "goodput"
        } else {
            "tokens/s"
        };
        out.push_str(&format!(
            "  🚀 Highest throughput:  {} — {:.0} {label} ({})\n",
            throughput.name,
            throughput.rank_throughput(),
            throughput.current_model
        ));
    }
    if let (Some(raw), Some(good)) = (
        profiles
            .iter()
            .max_by(|a, b| a.median_tokens_per_sec.total_cmp(&b.median_tokens_per_sec)),
        profiles
            .iter()
            .max_by(|a, b| a.rank_throughput().total_cmp(&b.rank_throughput())),
    ) {
        if raw.name != good.name {
            if let Some(rate) = raw.median_failure_rate {
                out.push_str(&format!(
                    "  {} leads on tokens/s ({:.0}) but loses on goodput because its failure rate is {:.0}%.\n",
                    raw.name,
                    raw.median_tokens_per_sec,
                    rate * 100.0
                ));
            }
        }
    }
    if let (Some(fast), Some(careful)) = (
        profiles
            .iter()
            .max_by(|a, b| a.rank_throughput().total_cmp(&b.rank_throughput())),
        profiles.iter().max_by(|a, b| {
            a.median_task_pass
                .unwrap_or(0.0)
                .total_cmp(&b.median_task_pass.unwrap_or(0.0))
        }),
    ) {
        if fast.name != careful.name {
            if let (Some(rate), Some(best)) = (fast.median_task_pass, careful.median_task_pass) {
                if best > rate {
                    out.push_str(&format!(
                        "  {} leads on goodput ({:.0}) but loses on task success because its pass rate is {:.0}%.\n",
                        fast.name,
                        fast.rank_throughput(),
                        rate * 100.0
                    ));
                }
            }
        }
    }
    if let Some(stable) = profiles
        .iter()
        .filter(|p| p.races >= 3)
        .min_by(|a, b| a.latency_cv.total_cmp(&b.latency_cv))
    {
        out.push_str(&format!(
            "  🎯 Most stable:  {} — {:.1}% latency variance across {} races\n",
            stable.name,
            stable.latency_cv * 100.0,
            stable.races
        ));
    }
    if let Some(cheapest) = profiles
        .iter()
        .filter_map(|p| p.cost_per_run_usd.map(|c| (p, c)))
        .min_by(|a, b| a.1.total_cmp(&b.1))
    {
        out.push_str(&format!(
            "  💰 Cheapest:  {} — ~${:.5}/run\n",
            cheapest.0.name, cheapest.1
        ));
    }

    out.push_str("\nProvider profiles:\n");
    let mut sorted = profiles.to_vec();
    sorted.sort_by(|a, b| a.median_latency_ms.total_cmp(&b.median_latency_ms));
    for p in &sorted {
        out.push_str(&format!("  {}\n", p.headline()));
        if let Some(vfm) = p.value_for_money {
            out.push_str(&format!("    value: {vfm:.0} tok/s per $/run\n"));
        }
        if let Some(cost) = p.cost_per_run_usd {
            out.push_str(&format!("    cost: ~${cost:.5}/run"));
            if let Some(monthly) = p.monthly_cost_usd {
                out.push_str(&format!(" (\u{2248}${monthly:.0}/mo at 1k req/day)"));
            }
            out.push('\n');
        }
    }

    // overall recommendation: goodput, with latency called out separately
    let mut by_goodput = profiles.to_vec();
    by_goodput.sort_by(|a, b| b.rank_throughput().total_cmp(&a.rank_throughput()));
    if let Some(best) = sorted.first() {
        out.push_str(&format!(
            "\n▸ For latency-sensitive workloads: use {}.\n",
            best.name
        ));
    }
    if let Some(best) = by_goodput.first() {
        out.push_str(&format!(
            "▸ For throughput: use {} ({:.0} goodput).\n",
            best.name,
            best.rank_throughput()
        ));
    }

    // note unpriced providers
    let unpriced: Vec<&ProviderProfile> = profiles
        .iter()
        .filter(|p| p.cost_per_run_usd.is_none())
        .collect();
    if !unpriced.is_empty() {
        out.push_str("\n  ℹ No pricing data for: ");
        for (i, p) in unpriced.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(&format!("{} ({})", p.name, p.current_model));
        }
        out.push_str("\n    pass `--price provider/model=in:out` to add cost data\n");
    }

    out.push_str(
        "\n> Based on recorded history; run `llmrace --record` to add more data points.\n",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bench::{LaneSummary, RunMetrics};

    #[test]
    fn tps_excludes_total_failure_records_and_vfm_matches_unit() {
        let mut ok = crate::history::record_from_summary(
            &LaneSummary {
                name: "a".into(),
                model: "m".into(),
                concurrency: 1,
                runs: vec![RunMetrics {
                    total_ms: 1100.0,
                    ttfb_ms: 100.0,
                    tokens: 100,
                    ..RunMetrics::default()
                }],
                ..LaneSummary::default()
            },
            10,
        );
        ok.timestamp_unix = 200;
        ok.cost_per_run_usd = Some(0.01);
        ok.failure_rate = Some(0.0);

        let mut failed = ok.clone();
        failed.timestamp_unix = 100;
        failed.mean_tokens_per_sec = 0.0;
        failed.failure_rate = Some(1.0);

        let profiles = build_profiles(&[ok, failed]);
        assert_eq!(profiles.len(), 1);
        assert!((profiles[0].median_tokens_per_sec - 100.0).abs() < 1e-6);
        assert!((profiles[0].value_for_money.expect("vfm") - 10_000.0).abs() < 1e-6);

        let rendered = render_recommendation(&profiles);
        assert!(rendered.contains("10000 tok/s per $/run"), "{rendered}");
    }
}
