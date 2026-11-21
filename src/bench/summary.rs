//! Aggregated per-lane statistics: everything derived from a lane's
//! measured rounds.

use super::metrics::RunMetrics;
use crate::stats::Sample;

/// Why a measured round produced no metrics.
///
/// The class is coarse on purpose: reports and alerts group timeouts and
/// rate limits without depending on provider error text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureClass {
    /// The request hit its deadline.
    Timeout,
    /// HTTP 429.
    RateLimit,
    /// HTTP 408, 409, 425 or 5xx.
    Server,
    /// Any other HTTP status.
    Client,
    /// The stream failed before a complete response (connection reset, decode).
    Stream,
    /// The provider broke the expected streaming protocol.
    Protocol,
    /// Local I/O failed.
    Io,
    /// The request was refused before it was sent.
    Config,
}

impl FailureClass {
    /// Classifies a round-ending error.
    ///
    /// A timeout is an [`crate::error::Error::Stream`] whose message contains
    /// `timed out`, which is the deadline error the engine raises.
    #[must_use]
    pub fn of(err: &crate::error::Error) -> Self {
        use crate::error::Error;
        match err {
            Error::Http { status: 429, .. } => FailureClass::RateLimit,
            Error::Http { status, .. }
                if (500..600).contains(status) || matches!(*status, 408 | 409 | 425) =>
            {
                FailureClass::Server
            }
            Error::Http { .. } => FailureClass::Client,
            Error::Stream(msg) if msg.contains("timed out") => FailureClass::Timeout,
            Error::Stream(_) => FailureClass::Stream,
            Error::Protocol(_) => FailureClass::Protocol,
            Error::Io(_) => FailureClass::Io,
            Error::Config(_) => FailureClass::Config,
        }
    }

    /// Stable label for JSON and tables.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            FailureClass::Timeout => "timeout",
            FailureClass::RateLimit => "rate_limit",
            FailureClass::Server => "server",
            FailureClass::Client => "client",
            FailureClass::Stream => "stream",
            FailureClass::Protocol => "protocol",
            FailureClass::Io => "io",
            FailureClass::Config => "config",
        }
    }
}

/// One measured round that produced no metrics after retries were exhausted.
#[derive(Debug, Clone, PartialEq)]
pub struct RoundFailure {
    /// Wall time of the attempt that failed, in milliseconds.
    ///
    /// Earlier attempts in the same round are counted in
    /// [`LaneSummary::retry_attempts`], not here.
    pub elapsed_ms: f64,
    /// Coarse cause.
    pub class: FailureClass,
    /// Attempts this round used, including the one that failed.
    pub attempts: u32,
    /// Truncated error text.
    pub message: String,
    /// Workload prompt id this round drew. Empty when the race was one string.
    pub prompt_id: String,
}

/// Aggregated results for one provider lane.
#[derive(Debug, Clone, Default)]
pub struct LaneSummary {
    /// Provider id.
    pub name: String,
    /// Model that was benchmarked.
    pub model: String,
    /// Successful measured rounds.
    ///
    /// Latency, tokens/s and cost are computed from these only. Failed
    /// measured rounds are [`Self::failures`] and stay out of those samples.
    pub runs: Vec<RunMetrics>,
    /// Measured rounds that failed after retries. Warmup failures are not
    /// recorded: warmup is outside the measured sample.
    pub failures: Vec<RoundFailure>,
    /// Extra attempts spent retrying measured rounds.
    ///
    /// This counts retries that later succeeded and retries that preceded
    /// a final failure. The attempt that produced metrics, and the attempt
    /// recorded in [`Self::failures`], are not included. Retries are not
    /// priced: a failed attempt does not return token usage.
    pub retry_attempts: u64,
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
            output: self.mean_tokens() + self.mean_extra_reasoning(),
        }
    }

    /// Mean reasoning tokens that are billed on top of completion tokens.
    /// Gemini thoughts are this case. OpenAI reasoning is already inside
    /// `tokens`, so it contributes nothing here.
    fn mean_extra_reasoning(&self) -> f64 {
        if self.runs.is_empty() {
            return 0.0;
        }
        let extra: u64 = self
            .runs
            .iter()
            .filter(|r| r.reasoning_billed_separately)
            .filter_map(|r| r.reasoning_tokens)
            .sum();
        extra as f64 / self.runs.len() as f64
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

    /// Reasoning tokens. Uses the provider count (`thoughtsTokenCount`,
    /// OpenAI `reasoning_tokens`) when any round carried one. Otherwise
    /// the visible-stream estimate: usage minus streamed tokens.
    #[must_use]
    pub fn reasoning_tokens(&self) -> u64 {
        let reported: Vec<u64> = self
            .runs
            .iter()
            .filter_map(|r| r.reasoning_tokens)
            .collect();
        if !reported.is_empty() {
            return reported.iter().sum();
        }
        self.total_tokens().saturating_sub(self.visible_tokens())
    }

    /// Time per output token, in milliseconds: the effective generation
    /// window divided by completion tokens. Median and p95 across rounds.
    /// This is the inverse of the decode rate inside tokens/s. `None` when
    /// no round produced tokens.
    #[must_use]
    pub fn tpot_ms(&self) -> Option<(f64, f64)> {
        let values: Vec<f64> = self
            .runs
            .iter()
            .filter(|r| r.tokens > 0)
            .map(|r| r.effective_gen_ms() / r.tokens as f64)
            .collect();
        if values.is_empty() {
            return None;
        }
        let sample = Sample::from_values(values);
        Some((sample.median(), sample.percentile(95.0)))
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

    /// Measured rounds, successful and failed.
    #[must_use]
    pub fn measured_rounds(&self) -> usize {
        self.runs.len() + self.failures.len()
    }

    /// Fraction of measured rounds that failed.
    ///
    /// `None` when the lane measured nothing (a stop before the first
    /// counted round). `Some(0.0)` when every measured round succeeded.
    #[must_use]
    pub fn failure_rate(&self) -> Option<f64> {
        let measured = self.measured_rounds();
        if measured == 0 {
            None
        } else {
            Some(self.failures.len() as f64 / measured as f64)
        }
    }

    /// Successful-round tokens/s scaled by the success rate.
    ///
    /// A lane at 100 tokens/s that failed half its rounds has a goodput of
    /// 50. With no failures this equals [`Self::mean_tokens_per_sec`], so
    /// a clean race ranks exactly as it did before failures were recorded.
    #[must_use]
    pub fn goodput_tokens_per_sec(&self) -> f64 {
        let success = 1.0 - self.failure_rate().unwrap_or(0.0);
        self.mean_tokens_per_sec() * success
    }

    /// `true` when at least one measured round succeeded.
    #[must_use]
    pub fn successful(&self) -> bool {
        self.error.is_none() && !self.runs.is_empty()
    }

    /// One row per workload prompt, in first-seen order. Empty when the
    /// race used a single prompt.
    #[must_use]
    pub fn prompt_rows(&self) -> Vec<PromptRow> {
        let mut rows: Vec<PromptRow> = Vec::new();
        for run in &self.runs {
            if run.prompt_id.is_empty() {
                continue;
            }
            if let Some(row) = rows.iter_mut().find(|r| r.id == run.prompt_id) {
                row.total_ms += run.total_ms;
                row.ttfb_ms += run.ttfb_ms;
                row.tokens_per_sec += run.tokens_per_sec();
                row.rounds += 1;
            } else {
                rows.push(PromptRow {
                    id: run.prompt_id.clone(),
                    rounds: 1,
                    total_ms: run.total_ms,
                    ttfb_ms: run.ttfb_ms,
                    tokens_per_sec: run.tokens_per_sec(),
                    task_pass: None,
                });
            }
        }
        for row in &mut rows {
            let n = row.rounds as f64;
            row.total_ms /= n;
            row.ttfb_ms /= n;
            row.tokens_per_sec /= n;
        }
        let checked: Vec<String> = rows
            .iter()
            .filter(|row| {
                self.runs
                    .iter()
                    .any(|run| run.prompt_id == row.id && run.task_ok.is_some())
            })
            .map(|row| row.id.clone())
            .collect();
        for id in checked {
            let ok = self
                .runs
                .iter()
                .filter(|run| run.prompt_id == id && run.task_ok == Some(true))
                .count();
            let failed = self
                .failures
                .iter()
                .filter(|failure| failure.prompt_id == id)
                .count();
            let measured = self.runs.iter().filter(|run| run.prompt_id == id).count() + failed;
            if let Some(row) = rows.iter_mut().find(|row| row.id == id) {
                row.task_pass = Some((ok, measured));
            }
        }
        rows
    }

    /// `(passed, measured)` for the task, when this lane had one.
    ///
    /// The denominator is every measured round, including HTTP failures,
    /// so a dropped round is not a pass. `None` when no round was checked.
    #[must_use]
    pub fn task_pass(&self) -> Option<(usize, usize)> {
        let checked = self.runs.iter().any(|r| r.task_ok.is_some());
        if !checked {
            return None;
        }
        let ok = self.runs.iter().filter(|r| r.task_ok == Some(true)).count();
        Some((ok, self.measured_rounds()))
    }

    /// Fraction of measured rounds that passed the task.
    #[must_use]
    pub fn task_rate(&self) -> Option<f64> {
        self.task_pass()
            .map(|(ok, n)| if n == 0 { 0.0 } else { ok as f64 / n as f64 })
    }
}

/// Aggregated timings for one workload prompt on one lane.
#[derive(Debug, Clone, PartialEq)]
pub struct PromptRow {
    /// Workload id.
    pub id: String,
    /// Successful rounds that used this prompt.
    pub rounds: usize,
    /// Mean total latency, milliseconds.
    pub total_ms: f64,
    /// Mean time to first token, milliseconds.
    pub ttfb_ms: f64,
    /// Mean tokens per second.
    pub tokens_per_sec: f64,
    /// `(passed, measured)` when this prompt was checked. The denominator
    /// includes an HTTP failure of this id, so a dropped round is not a pass.
    /// `None` when no successful round of this prompt had a task.
    pub task_pass: Option<(usize, usize)>,
}
