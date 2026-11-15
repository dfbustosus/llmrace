//! Aggregated per-lane statistics: everything derived from a lane's
//! measured rounds.

use super::metrics::RunMetrics;
use crate::stats::Sample;

/// Aggregated results for one provider lane.
#[derive(Debug, Clone, Default)]
pub struct LaneSummary {
    /// Provider id.
    pub name: String,
    /// Model that was benchmarked.
    pub model: String,
    /// Measured rounds.
    pub runs: Vec<RunMetrics>,
    /// Fatal lane error, when no round succeeded.
    pub error: Option<String>,
    /// Prompt used (for token estimation in cost calculations).
    pub prompt: String,
    /// Concurrency level of the rounds.
    pub concurrency: usize,
    /// Prompt variant label when running a sweep, otherwise `None`.
    pub variant: Option<String>,
    /// Tool task name when run with `--tools`, used to pick the right
    /// argument validator for `tool_validity`.
    pub tool_task: Option<String>,
}

impl LaneSummary {
    fn total_sample(&self) -> Sample {
        Sample::from_values(self.runs.iter().map(|r| r.total_ms).collect())
    }

    /// Mean of round total latencies.
    #[must_use]
    pub fn mean_total_ms(&self) -> f64 {
        self.total_sample().mean()
    }

    /// Median of round total latencies.
    #[must_use]
    pub fn median_total_ms(&self) -> f64 {
        self.total_sample().median()
    }

    /// p95 of round total latencies (linear interpolation).
    #[must_use]
    pub fn p95_total_ms(&self) -> f64 {
        self.total_sample().percentile(95.0)
    }

    /// 95% confidence interval of the mean total latency.
    #[must_use]
    pub fn ci95_total_ms(&self) -> Option<(f64, f64)> {
        self.total_sample().t_ci95()
    }

    /// Mean time-to-first-byte.
    #[must_use]
    pub fn mean_ttfb_ms(&self) -> f64 {
        if self.runs.is_empty() {
            return 0.0;
        }
        Sample::from_values(self.runs.iter().map(|r| r.ttfb_ms).collect()).mean()
    }

    /// Mean completion tokens per round.
    #[must_use]
    pub fn mean_tokens(&self) -> f64 {
        if self.runs.is_empty() {
            return 0.0;
        }
        self.runs.iter().map(|r| r.tokens as f64).sum::<f64>() / self.runs.len() as f64
    }

    /// Aggregate throughput: total tokens over the summed effective
    /// generation window.
    #[must_use]
    pub fn mean_tokens_per_sec(&self) -> f64 {
        if self.runs.is_empty() {
            return 0.0;
        }
        let total_tokens: f64 = self.runs.iter().map(|r| r.tokens as f64).sum();
        let total_gen: f64 = self
            .runs
            .iter()
            .map(|r| r.effective_gen_ms() / 1000.0)
            .sum();
        if total_gen <= 0.0 {
            0.0
        } else {
            total_tokens / total_gen
        }
    }

    /// Standard percentile ladder of round total latencies:
    /// p10, p25, p50, p75, p90, p95, p99 (linear interpolation, one sort).
    #[must_use]
    pub fn latency_percentiles(&self) -> Vec<(&'static str, f64)> {
        let sample =
            crate::stats::Sample::from_values(self.runs.iter().map(|r| r.total_ms).collect());
        let ps = [10.0, 25.0, 50.0, 75.0, 90.0, 95.0, 99.0];
        let values = sample.percentiles(&ps);
        let labels = ["p10", "p25", "p50", "p75", "p90", "p95", "p99"];
        labels.into_iter().zip(values).collect()
    }

    /// Indices of rounds whose total latency is a 3-sigma outlier.
    #[must_use]
    pub fn outlier_runs(&self) -> Vec<usize> {
        self.total_sample().outliers_3sigma()
    }

    /// Estimated cost per round in USD, when the model has known pricing.
    #[must_use]
    pub fn cost_per_run_usd(&self) -> Option<f64> {
        let (pin, pout) = crate::cost::pricing(&self.name, &self.model)?;
        let prompt_tokens = crate::cost::estimate_prompt_tokens(&self.prompt);
        Some((self.mean_tokens() * pout + prompt_tokens * pin) / 1_000_000.0)
    }

    /// `(valid, total)` tool-argument validity across rounds that produced
    /// arguments; `None` when no round produced tool arguments. Validation
    /// uses this lane's tool task when known, falling back to the default.
    #[must_use]
    pub fn tool_validity(&self) -> Option<(usize, usize)> {
        let with_args: Vec<&RunMetrics> = self
            .runs
            .iter()
            .filter(|r| !r.tool_args.is_empty())
            .collect();
        if with_args.is_empty() {
            return None;
        }
        let validate = match self.tool_task.as_deref().and_then(crate::tools::find) {
            Some(task) => task.validate,
            None => crate::tools::validate_args,
        };
        let valid = with_args.iter().filter(|r| validate(&r.tool_args)).count();
        Some((valid, with_args.len()))
    }

    /// Median of provider-reported server-side processing time in ms.
    #[must_use]
    pub fn median_server_ms(&self) -> Option<f64> {
        let values: Vec<f64> = self.runs.iter().filter_map(|r| r.server_ms).collect();
        if values.is_empty() {
            return None;
        }
        Some(Sample::from_values(values).median())
    }

    /// p50 of inter-token gaps across all measured rounds; `None` when no
    /// gap was observed (single-token responses).
    #[must_use]
    pub fn itl_p50_ms(&self) -> Option<f64> {
        self.itl_sample().map(|s| s.percentile(50.0))
    }

    /// p95 of inter-token gaps across all measured rounds; `None` when no
    /// gap was observed.
    #[must_use]
    pub fn itl_p95_ms(&self) -> Option<f64> {
        self.itl_sample().map(|s| s.percentile(95.0))
    }

    /// Total stutters (inter-token gaps over [`STUTTER_MS`]) across rounds.
    #[must_use]
    pub fn stutters(&self) -> u64 {
        self.runs.iter().map(|r| r.stutters).sum()
    }

    /// Estimated visible (streamed) tokens across rounds.
    #[must_use]
    pub fn visible_tokens(&self) -> u64 {
        self.runs.iter().map(|r| r.visible_tokens).sum()
    }

    /// Hidden reasoning tokens: provider-reported usage minus the visible
    /// stream estimate. Zero for classic models and providers without
    /// usage reporting.
    #[must_use]
    pub fn reasoning_tokens(&self) -> u64 {
        self.total_tokens().saturating_sub(self.visible_tokens())
    }

    /// Total provider-reported tokens across rounds.
    #[must_use]
    pub fn total_tokens(&self) -> u64 {
        self.runs.iter().map(|r| r.tokens).sum()
    }

    /// Estimated server-side thinking time for reasoning models: the
    /// provider-reported processing window scaled by the hidden-reasoning
    /// token share. `None` without server timing or for classic models.
    ///
    /// This is an estimate, not a measurement — providers do not report
    /// the reasoning/visible split of their processing time. Assuming a
    /// uniform server-side token rate, the reasoning share of the window
    /// is `reasoning_tokens / total_tokens`.
    #[must_use]
    pub fn thinking_ms_estimate(&self) -> Option<f64> {
        let server = self.median_server_ms()?;
        let total = self.total_tokens();
        let hidden = self.reasoning_tokens();
        if total == 0 || hidden == 0 {
            return None;
        }
        Some(server * hidden as f64 / total as f64)
    }

    fn itl_sample(&self) -> Option<Sample> {
        let gaps: Vec<f64> = self
            .runs
            .iter()
            .flat_map(|r| r.itl_gaps_ms.iter().copied())
            .collect();
        if gaps.is_empty() {
            None
        } else {
            Some(Sample::from_values(gaps))
        }
    }

    /// Last provider request ID.
    #[must_use]
    pub fn last_request_id(&self) -> Option<&str> {
        self.runs.iter().rev().find_map(|r| r.request_id.as_deref())
    }

    /// Estimated network overhead: measured TTFB minus provider-reported server time.
    #[must_use]
    pub fn network_overhead_ms(&self) -> Option<f64> {
        let server = self.median_server_ms()?;
        Some(self.mean_ttfb_ms() - server)
    }

    /// `true` when at least one measured round succeeded.
    #[must_use]
    pub fn successful(&self) -> bool {
        self.error.is_none() && !self.runs.is_empty()
    }
}
