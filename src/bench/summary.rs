//! Aggregated per-lane statistics: everything derived from a lane's
//! measured rounds.

use super::metrics::RunMetrics;
use crate::stats::Sample;

/// Why a measured round produced no metrics.
///
/// The class is coarse on purpose: reports and alerts group timeouts and
/// rate limits without depending on provider error text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
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
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct RoundFailure {
    /// Whether this request had an answer or tool-call check.
    pub task_checked: bool,
    /// Individual requests when this is a fully failed round. Empty for
    /// individual attempts and legacy manually constructed failures.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub requests: Vec<super::metrics::RequestOutcome>,
    /// Wall time in milliseconds. Individual failures measure one attempt;
    /// a fully failed round includes all attempts and retry delays.
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
    /// Measured rounds with at least one successful request.
    ///
    /// Latency, tokens/s and cost are computed from these only.
    /// Fully failed rounds are [`Self::failures`] and stay out of those samples.
    /// Partial failures are retained in each run's request outcomes and
    /// counted by [`Self::failed_rounds`].
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
    /// Per-race price overrides. When set, cost lookups consult this book
    /// and the built-in table only — the process-global override map is
    /// ignored, so parallel races cannot leak prices into each other.
    pub price_book: Option<crate::cost::PriceBook>,
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

    fn request_ttfb_sample(&self) -> Option<Sample> {
        let mut values = Vec::new();
        for r in &self.runs {
            if !r.requests.is_empty() {
                values.extend(r.requests.iter().filter_map(|q| q.effective_ttfb_ms()));
            } else if r.ttfb_ms.is_finite() && r.ttfb_ms >= 0.0 {
                values.push(r.ttfb_ms);
            }
        }
        (!values.is_empty()).then(|| Sample::from_values(values))
    }

    /// p50 per-request effective time-to-first-byte across successful requests
    /// in measured rounds; `None` when no valid observations exist.
    ///
    /// Under `--concurrency N` (`N > 1`), this captures the per-request median
    /// (including queueing and retry delays) rather than the round minimum.
    #[must_use]
    pub fn request_ttfb_p50_ms(&self) -> Option<f64> {
        self.request_ttfb_sample().map(|s| s.percentile(50.0))
    }

    /// p95 per-request effective time-to-first-byte across successful requests
    /// in measured rounds; `None` when no valid observations exist.
    ///
    /// Under `--concurrency N` (`N > 1`), this surfaces queueing tails that the
    /// round-minimum [`Self::ttfb_p95_ms`] hides.
    #[must_use]
    pub fn request_ttfb_p95_ms(&self) -> Option<f64> {
        self.request_ttfb_sample().map(|s| s.percentile(95.0))
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

    fn mean_successful_requests_per_round(&self) -> f64 {
        if self.runs.is_empty() {
            return self.concurrency.max(1) as f64;
        }
        let total: usize = self
            .runs
            .iter()
            .map(|r| {
                if r.requests.is_empty() {
                    self.concurrency.max(1)
                } else {
                    r.requests.iter().filter(|q| q.metrics().is_some()).count()
                }
            })
            .sum();
        total as f64 / self.runs.len() as f64
    }

    fn priced_usage(&self) -> crate::cost::UsageTokens {
        let measured = self.prompt_tokens_measured();
        let bucket = |mean: Option<f64>| if measured { mean.unwrap_or(0.0) } else { 0.0 };
        crate::cost::UsageTokens {
            prompt: self.mean_prompt_tokens().unwrap_or_else(|| {
                crate::cost::estimate_prompt_tokens(&self.prompt)
                    * self.mean_successful_requests_per_round()
            }),
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
        let rates =
            crate::cost::token_rates_for(self.price_book.as_ref(), &self.name, &self.model)?;
        Some(crate::cost::usage_cost_usd(rates, self.priced_usage()))
    }

    /// `false` when the lane reported cache tokens and the price table has
    /// no rate for that bucket, so those tokens were billed at the input rate.
    #[must_use]
    pub fn cache_discount_applied(&self) -> bool {
        match crate::cost::token_rates_for(self.price_book.as_ref(), &self.name, &self.model) {
            Some(rates) => crate::cost::cache_rates_cover(rates, self.priced_usage()),
            None => true,
        }
    }

    /// `(valid, total)` tool-argument validity across rounds that produced
    /// arguments; `None` when no round produced tool arguments. Validation
    /// honors workload-evaluated `task_ok` for workload rounds (which may
    /// carry custom tool schemas), and otherwise uses this lane's built-in
    /// tool task validator, falling back to the default.
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
        let valid = with_args
            .iter()
            .filter(|r| {
                if !r.prompt_id.is_empty() {
                    if let Some(ok) = r.task_ok {
                        return ok;
                    }
                }
                validate(&r.tool_args)
            })
            .count();
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

    /// Median of provider-reported end-to-end server duration in milliseconds
    /// (e.g., Bedrock `metadata.metrics.latencyMs`); `None` when no round
    /// reported one.
    #[must_use]
    pub fn median_server_total_ms(&self) -> Option<f64> {
        let values: Vec<f64> = self
            .runs
            .iter()
            .filter_map(|r| r.server_total_ms)
            .filter(|v| v.is_finite() && *v >= 0.0)
            .collect();
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

    /// Total stutters (inter-token gaps over [`super::STUTTER_MS`]) across rounds.
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
    /// falls back to the visible-stream estimate (`usage - visible_tokens`)
    /// only for known reasoning models.
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
        if crate::provider::is_reasoning_model(&self.model) {
            self.total_tokens().saturating_sub(self.visible_tokens())
        } else {
            0
        }
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
    /// is `reasoning_tokens / (total_tokens + extra_reasoning_tokens)`,
    /// clamped to `<= 1.0`.
    #[must_use]
    pub fn thinking_ms_estimate(&self) -> Option<f64> {
        let server = self.median_server_ms()?;
        let total = self.total_tokens();
        let extra_reasoning: u64 = self
            .runs
            .iter()
            .filter(|r| r.reasoning_billed_separately)
            .filter_map(|r| r.reasoning_tokens)
            .sum();
        let denom = total.saturating_add(extra_reasoning);
        let hidden = self.reasoning_tokens();
        if denom == 0 || hidden == 0 {
            return None;
        }
        let ratio = (hidden as f64 / denom as f64).min(1.0);
        Some(server * ratio)
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

    /// Rounds with at least one request that failed after retries.
    pub fn failed_rounds(&self) -> usize {
        self.failures.len()
            + self
                .runs
                .iter()
                .filter(|r| r.requests.iter().any(|q| q.failure().is_some()))
                .count()
    }

    /// Measured requests, excluding retry attempts.
    pub fn measured_requests(&self) -> usize {
        self.runs
            .iter()
            .map(|r| r.requests.len().max(self.concurrency.max(1)))
            .sum::<usize>()
            + self
                .failures
                .iter()
                .map(|f| f.requests.len().max(self.concurrency.max(1)))
                .sum::<usize>()
    }

    /// Requests whose final attempt failed.
    pub fn failed_requests(&self) -> usize {
        self.runs
            .iter()
            .flat_map(|r| &r.requests)
            .filter(|q| q.failure().is_some())
            .count()
            + self
                .failures
                .iter()
                .map(|f| f.requests.len().max(self.concurrency.max(1)))
                .sum::<usize>()
    }

    /// Fraction of requests that failed, distinct from the round failure rate.
    pub fn request_failure_rate(&self) -> Option<f64> {
        let n = self.measured_requests();
        (n > 0).then(|| self.failed_requests() as f64 / n as f64)
    }

    /// Output tokens from successful requests per elapsed second, including
    /// failed rounds and retry delays in the denominator.
    pub fn end_to_end_tokens_per_sec(&self) -> f64 {
        let ms = self.runs.iter().map(|r| r.total_ms).sum::<f64>()
            + self.failures.iter().map(|f| f.elapsed_ms).sum::<f64>();
        if ms > 0.0 {
            self.total_tokens() as f64 * 1000.0 / ms
        } else {
            0.0
        }
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
            Some(self.failed_rounds() as f64 / measured as f64)
        }
    }

    /// Successful-round tokens/s scaled by the fraction of measured rounds
    /// that produced output (`runs.len() / measured_rounds`).
    ///
    /// For `concurrency == 1` this is identically `tokens/s * (1 - failure_rate)`.
    /// Under `concurrency > 1`, partially-failed rounds already reflect only
    /// successful requests' tokens over full wall-time, so scaling by
    /// `runs.len() / measured` avoids double-penalizing partial failures to zero.
    #[must_use]
    pub fn goodput_tokens_per_sec(&self) -> f64 {
        let measured = self.measured_rounds();
        if measured == 0 {
            return 0.0;
        }
        self.mean_tokens_per_sec() * (self.runs.len() as f64 / measured as f64)
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
                row.failed_rounds +=
                    usize::from(run.requests.iter().any(|r| r.failure().is_some()));
            } else {
                rows.push(PromptRow {
                    id: run.prompt_id.clone(),
                    rounds: 1,
                    failed_rounds: usize::from(run.requests.iter().any(|r| r.failure().is_some())),
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
        for failure in &self.failures {
            if failure.prompt_id.is_empty() {
                continue;
            }
            if let Some(row) = rows.iter_mut().find(|row| row.id == failure.prompt_id) {
                row.failed_rounds += 1;
            } else {
                rows.push(PromptRow {
                    id: failure.prompt_id.clone(),
                    rounds: 0,
                    failed_rounds: 1,
                    total_ms: 0.0,
                    ttfb_ms: 0.0,
                    tokens_per_sec: 0.0,
                    task_pass: None,
                });
            }
        }
        let checked: Vec<String> = rows
            .iter()
            .filter(|row| {
                self.runs
                    .iter()
                    .any(|run| run.prompt_id == row.id && run.task_ok.is_some())
                    || self
                        .failures
                        .iter()
                        .any(|f| f.prompt_id == row.id && f.task_checked)
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

    /// Measured rounds that hit the provider's prefix cache.
    ///
    /// A lane where this is between 1 and `runs.len() − 1` mixes cold and
    /// warm rounds: the latency sample is not homogeneous, and the cost of
    /// cached input is discounted only on the warm rounds.
    #[must_use]
    pub fn cache_hit_rounds(&self) -> usize {
        self.runs.iter().filter(|r| r.cache_read_tokens > 0).count()
    }

    /// `(passed, checked)` for the task, when this lane had one.
    ///
    /// The denominator counts only checked rounds (rounds with `task_ok` set
    /// plus failed rounds with `task_checked == true`), so unchecked prompts in
    /// mixed workloads are not counted as failures while dropped checked rounds
    /// still count as failures. `None` when no round was checked.
    #[must_use]
    pub fn task_pass(&self) -> Option<(usize, usize)> {
        let checked_runs = self.runs.iter().filter(|r| r.task_ok.is_some()).count();
        let checked_failures = self.failures.iter().filter(|f| f.task_checked).count();
        let total = checked_runs + checked_failures;
        if total == 0 {
            return None;
        }
        let ok = self.runs.iter().filter(|r| r.task_ok == Some(true)).count();
        Some((ok, total))
    }

    /// Fraction of checked rounds that passed the task.
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
    /// Rounds with at least one failed request, including fully failed rounds.
    pub failed_rounds: usize,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bench::metrics::{RequestAttempt, RequestOutcome};

    #[test]
    fn goodput_under_concurrency_partial_failure_is_not_zeroed() {
        let ok_req = RequestOutcome {
            elapsed_ms: 1000.0,
            attempts: vec![RequestAttempt::Success(RunMetrics {
                total_ms: 1000.0,
                ttfb_ms: 100.0,
                tokens: 90,
                ..RunMetrics::default()
            })],
        };
        let fail_req = RequestOutcome {
            elapsed_ms: 1000.0,
            attempts: vec![RequestAttempt::Failure(RoundFailure {
                task_checked: false,
                requests: Vec::new(),
                elapsed_ms: 1000.0,
                class: FailureClass::RateLimit,
                attempts: 1,
                message: "429".into(),
                prompt_id: String::new(),
            })],
        };
        let summary = LaneSummary {
            name: "p".into(),
            model: "m".into(),
            concurrency: 2,
            runs: vec![RunMetrics {
                requests: vec![ok_req, fail_req],
                total_ms: 1100.0,
                ttfb_ms: 100.0,
                tokens: 90,
                ..RunMetrics::default()
            }],
            ..LaneSummary::default()
        };
        // Round failure rate is 1.0 (1/1 round had a failed request),
        // yet goodput must equal the round's actual surviving tokens/s (90 tok/s), not 0.
        assert_eq!(summary.failure_rate(), Some(1.0));
        assert!((summary.goodput_tokens_per_sec() - 90.0).abs() < 1e-6);
    }

    #[test]
    fn thinking_ms_estimate_handles_separately_billed_reasoning_tokens() {
        let gemini = LaneSummary {
            name: "gemini".into(),
            model: "gemini-2.5-flash".into(),
            runs: vec![RunMetrics {
                total_ms: 1200.0,
                ttfb_ms: 200.0,
                tokens: 200,
                reasoning_tokens: Some(800),
                reasoning_billed_separately: true,
                server_ms: Some(1000.0),
                ..RunMetrics::default()
            }],
            ..LaneSummary::default()
        };
        let est = gemini.thinking_ms_estimate().expect("estimate");
        assert!(
            (est - 800.0).abs() < 1e-6,
            "expected 1000 * 800 / (200 + 800) = 800, got {est}"
        );
    }

    #[test]
    fn task_pass_excludes_unchecked_rounds_in_mixed_workloads() {
        let summary = LaneSummary {
            name: "p".into(),
            model: "m".into(),
            runs: vec![
                RunMetrics {
                    prompt_id: "checked".into(),
                    task_ok: Some(true),
                    ..RunMetrics::default()
                },
                RunMetrics {
                    prompt_id: "unchecked".into(),
                    task_ok: None,
                    ..RunMetrics::default()
                },
            ],
            failures: vec![
                RoundFailure {
                    task_checked: true,
                    requests: Vec::new(),
                    elapsed_ms: 100.0,
                    class: FailureClass::Server,
                    attempts: 1,
                    message: "500".into(),
                    prompt_id: "checked".into(),
                },
                RoundFailure {
                    task_checked: false,
                    requests: Vec::new(),
                    elapsed_ms: 100.0,
                    class: FailureClass::Server,
                    attempts: 1,
                    message: "500".into(),
                    prompt_id: "unchecked".into(),
                },
            ],
            ..LaneSummary::default()
        };
        assert_eq!(summary.task_pass(), Some((1, 2)));
    }

    #[test]
    fn tool_validity_honors_custom_workload_tool_task_ok() {
        let summary = LaneSummary {
            name: "p".into(),
            model: "m".into(),
            tool_task: Some("weather".into()),
            runs: vec![RunMetrics {
                prompt_id: "custom_order".into(),
                tool_args: r#"{"order_id":"ORD-42"}"#.into(),
                task_ok: Some(true),
                ..RunMetrics::default()
            }],
            ..LaneSummary::default()
        };
        assert_eq!(summary.tool_validity(), Some((1, 1)));
    }

    #[test]
    fn reasoning_tokens_gates_fallback_on_reasoning_model_only() {
        let classic = LaneSummary {
            name: "openai".into(),
            model: "gpt-4o-mini".into(),
            runs: vec![RunMetrics {
                tokens: 500,
                visible_tokens: 60,
                ..RunMetrics::default()
            }],
            ..LaneSummary::default()
        };
        assert_eq!(classic.reasoning_tokens(), 0);

        let gpt5 = LaneSummary {
            model: "gpt-5-nano".into(),
            ..classic.clone()
        };
        assert_eq!(gpt5.reasoning_tokens(), 440);

        // Explicit provider-reported reasoning_tokens works on any model name.
        let gemini = LaneSummary {
            name: "gemini".into(),
            model: "gemini-2.5-flash".into(),
            runs: vec![RunMetrics {
                tokens: 100,
                visible_tokens: 100,
                reasoning_tokens: Some(250),
                ..RunMetrics::default()
            }],
            ..LaneSummary::default()
        };
        assert_eq!(gemini.reasoning_tokens(), 250);
    }

    #[test]
    fn unmeasured_prompt_cost_scales_by_concurrency() {
        let single = LaneSummary {
            name: "openai".into(),
            model: "gpt-4o-mini".into(),
            prompt: "Explain zero-knowledge proofs briefly.".into(),
            concurrency: 1,
            runs: vec![RunMetrics {
                tokens: 100,
                ..RunMetrics::default()
            }],
            ..LaneSummary::default()
        };
        let quad = LaneSummary {
            concurrency: 4,
            ..single.clone()
        };
        assert!((quad.priced_usage().prompt - single.priced_usage().prompt * 4.0).abs() < 1e-9);
    }

    #[test]
    fn request_ttfb_percentiles_expose_concurrency_queueing_tail_and_retries() {
        let make_req = |ttfb_ms: f64, total_ms: f64, elapsed_ms: f64| RequestOutcome {
            elapsed_ms,
            attempts: vec![RequestAttempt::Success(RunMetrics {
                total_ms,
                ttfb_ms,
                tokens: 10,
                ..RunMetrics::default()
            })],
        };
        // Round has 4 concurrent requests: fast (50ms), mid (80ms, 120ms), and queued/retried tail
        // (attempt total 200ms, ttfb 40ms, total elapsed including retry 600ms -> effective ttfb 440ms).
        let summary = LaneSummary {
            name: "p".into(),
            model: "m".into(),
            concurrency: 4,
            runs: vec![RunMetrics {
                requests: vec![
                    make_req(50.0, 200.0, 200.0),
                    make_req(80.0, 220.0, 220.0),
                    make_req(120.0, 260.0, 260.0),
                    make_req(40.0, 200.0, 600.0), // effective_ttfb = 40 + (600 - 200) = 440
                ],
                total_ms: 600.0,
                ttfb_ms: 50.0, // round minimum
                tokens: 40,
                ..RunMetrics::default()
            }],
            ..LaneSummary::default()
        };
        assert_eq!(summary.ttfb_p95_ms(), Some(50.0));
        // Sorted effective TTFBs: [50.0, 80.0, 120.0, 440.0]
        // p50 = 100.0; p95: idx = 0.95 * 3 = 2.85 -> 120 + 0.85 * 320 = 392.0
        let p50 = summary.request_ttfb_p50_ms().expect("p50");
        let p95 = summary.request_ttfb_p95_ms().expect("p95");
        assert!((p50 - 100.0).abs() < 1e-9, "got {p50}");
        assert!((p95 - 392.0).abs() < 1e-9, "got {p95}");
        assert!(p95 > summary.ttfb_p95_ms().expect("round p95"));
    }

    #[test]
    fn request_ttfb_falls_back_to_round_ttfb_when_requests_empty() {
        let summary = LaneSummary {
            name: "p".into(),
            model: "m".into(),
            concurrency: 1,
            runs: vec![
                RunMetrics {
                    ttfb_ms: 20.0,
                    ..RunMetrics::default()
                },
                RunMetrics {
                    ttfb_ms: 40.0,
                    ..RunMetrics::default()
                },
                RunMetrics {
                    ttfb_ms: f64::NAN,
                    ..RunMetrics::default()
                },
            ],
            ..LaneSummary::default()
        };
        assert_eq!(summary.request_ttfb_p50_ms(), Some(30.0));
        let p95 = summary.request_ttfb_p95_ms().expect("p95");
        assert!((p95 - 39.0).abs() < 1e-9, "got {p95}");

        let empty = LaneSummary::default();
        assert_eq!(empty.request_ttfb_p50_ms(), None);
        assert_eq!(empty.request_ttfb_p95_ms(), None);
    }

    #[test]
    fn median_server_total_ms_aggregates_present_values_only() {
        let none = LaneSummary {
            runs: vec![RunMetrics::default()],
            ..LaneSummary::default()
        };
        assert_eq!(none.median_server_total_ms(), None);

        let present = LaneSummary {
            runs: vec![
                RunMetrics {
                    server_total_ms: Some(300.0),
                    ..RunMetrics::default()
                },
                RunMetrics {
                    server_total_ms: None,
                    ..RunMetrics::default()
                },
                RunMetrics {
                    server_total_ms: Some(500.0),
                    ..RunMetrics::default()
                },
                RunMetrics {
                    server_total_ms: Some(400.0),
                    ..RunMetrics::default()
                },
            ],
            ..LaneSummary::default()
        };
        assert_eq!(present.median_server_total_ms(), Some(400.0));
    }
}
