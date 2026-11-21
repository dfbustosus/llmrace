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

    /// p50 time-to-first-byte across rounds; `None` with no runs.
    #[must_use]
    pub fn ttfb_p50_ms(&self) -> Option<f64> {
        self.ttfb_sample().map(|s| s.percentile(50.0))
    }

    /// p95 time-to-first-byte across rounds — the tail first-token
    /// latency; `None` with no runs.
    #[must_use]
    pub fn ttfb_p95_ms(&self) -> Option<f64> {
        self.ttfb_sample().map(|s| s.percentile(95.0))
    }

    fn ttfb_sample(&self) -> Option<crate::stats::Sample> {
        (!self.runs.is_empty()).then(|| {
            crate::stats::Sample::from_values(self.runs.iter().map(|r| r.ttfb_ms).collect())
        })
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

    /// Mean of `pick` over rounds that reported a prompt-token count.
    ///
    /// `None` when no round reported usage. The denominator is the number of
    /// rounds that reported, so a round with a measured prompt and zero
    /// cache tokens still counts.
    fn mean_measured(&self, pick: impl Fn(&RunMetrics) -> u64) -> Option<f64> {
        let measured: Vec<f64> = self
            .runs
            .iter()
            .filter(|r| r.prompt_tokens > 0)
            .map(|r| pick(r) as f64)
            .collect();
        if measured.is_empty() {
            None
        } else {
            Some(measured.iter().sum::<f64>() / measured.len() as f64)
        }
    }

    /// Mean provider-reported prompt tokens per round.
    ///
    /// `None` when no round reported usage. With `--concurrency N` a round's
    /// count is the sum across the N requests, matching [`Self::mean_tokens`].
    #[must_use]
    pub fn mean_prompt_tokens(&self) -> Option<f64> {
        self.mean_measured(|r| r.prompt_tokens)
    }

    /// Mean cache-read tokens per measured round. `Some(0.0)` when usage was
    /// reported and no token was a cache read.
    #[must_use]
    pub fn mean_cache_read_tokens(&self) -> Option<f64> {
        self.mean_measured(|r| r.cache_read_tokens)
    }

    /// Mean short-TTL cache-write tokens per measured round.
    #[must_use]
    pub fn mean_cache_write_tokens(&self) -> Option<f64> {
        self.mean_measured(|r| r.cache_write_tokens)
    }

    /// Mean 1-hour cache-write tokens per measured round.
    #[must_use]
    pub fn mean_cache_write_1h_tokens(&self) -> Option<f64> {
        self.mean_measured(|r| r.cache_write_1h_tokens)
    }

    /// `true` when cost can use billed input tokens rather than the
    /// character estimate of the user prompt.
    #[must_use]
    pub fn prompt_tokens_measured(&self) -> bool {
        self.mean_prompt_tokens().is_some()
    }

    fn priced_usage(&self) -> crate::cost::UsageTokens {
        let measured = self.prompt_tokens_measured();
        let bucket = |mean: Option<f64>| if measured { mean.unwrap_or(0.0) } else { 0.0 };
        crate::cost::UsageTokens {
            prompt: self
                .mean_prompt_tokens()
                .unwrap_or_else(|| crate::cost::estimate_prompt_tokens(&self.prompt)),
            cache_read: bucket(self.mean_cache_read_tokens()),
            cache_write: bucket(self.mean_cache_write_tokens()),
            cache_write_1h: bucket(self.mean_cache_write_1h_tokens()),
            output: self.mean_tokens(),
        }
    }

    /// Cost per round in USD, when the model has known pricing.
    ///
    /// Input tokens are the provider-reported count when any round carried
    /// usage (that count includes the system prompt, tool schemas, cache
    /// reads and cache writes). Cache buckets use the published cached-input
    /// rate when this crate has one, and the uncached input rate otherwise.
    /// When the provider omits usage, the user prompt is estimated and the
    /// markdown report marks the figure with `~`. Hourly cache storage is
    /// not part of the per-request usage and is not included.
    #[must_use]
    pub fn cost_per_run_usd(&self) -> Option<f64> {
        let rates = crate::cost::token_rates(&self.name, &self.model)?;
        Some(crate::cost::usage_cost_usd(rates, self.priced_usage()))
    }

    /// `false` when the lane reported cache tokens and the price table has
    /// no rate for that bucket, so those tokens were billed at the input rate.
    #[must_use]
    pub fn cache_discount_applied(&self) -> bool {
        match crate::cost::token_rates(&self.name, &self.model) {
            Some(rates) => crate::cost::cache_rates_cover(rates, self.priced_usage()),
            None => true,
        }
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
