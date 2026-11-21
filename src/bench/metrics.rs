//! Benchmark data types: per-run measurements, lane tuning and live
//! progress updates.

use std::sync::Arc;

/// Raw measurements from a single streamed completion.
#[derive(Debug, Clone, Default)]
pub struct RunMetrics {
    /// Wall-clock time from request to last token, in milliseconds.
    pub total_ms: f64,
    /// Time from request to first token, in milliseconds.
    pub ttfb_ms: f64,
    /// Completion tokens (provider-reported usage when available).
    pub tokens: u64,
    /// Sum of token byte lengths.
    pub chars: u64,
    /// Provider-reported prompt tokens. `0` when the provider did not
    /// report usage; cost then falls back to a text estimate. Includes
    /// cache reads and cache writes.
    pub prompt_tokens: u64,
    /// Cache-read tokens included in [`Self::prompt_tokens`]. `0` when the
    /// provider reported none.
    pub cache_read_tokens: u64,
    /// Short-TTL cache-write tokens included in [`Self::prompt_tokens`].
    pub cache_write_tokens: u64,
    /// 1-hour cache-write tokens included in [`Self::prompt_tokens`].
    pub cache_write_1h_tokens: u64,
    /// Provider-reported reasoning tokens. `None` when the provider sent
    /// no count; the lane then estimates them from the visible stream.
    pub reasoning_tokens: Option<u64>,
    /// `true` when [`Self::reasoning_tokens`] is billed on top of [`Self::tokens`].
    pub reasoning_billed_separately: bool,
    /// Accumulated tool-call argument JSON (empty when the run had none).
    pub tool_args: String,
    /// Workload prompt id for this round. Empty when the race used one prompt.
    pub prompt_id: String,
    /// Whether the response passed the task. `None` when the round had no
    /// task (no `--tools` and no `expect`). A `false` is still a successful
    /// HTTP call.
    pub task_ok: Option<bool>,
    /// Provider-reported server-side processing time in milliseconds,
    /// from response headers (e.g. OpenAI's `openai-processing-ms`).
    pub server_ms: Option<f64>,
    /// Provider request ID for correlation with provider dashboards.
    pub request_id: Option<String>,
    /// Remaining rate-limit requests after this call.
    pub ratelimit_remaining: Option<String>,
    /// Inter-token gaps in milliseconds (time between consecutive streamed
    /// tokens); TTFB itself is not included.
    pub itl_gaps_ms: Vec<f64>,
    /// Count of inter-token gaps exceeding [`STUTTER_MS`].
    pub stutters: u64,
    /// Estimated tokens actually streamed as visible content (each SSE
    /// delta may carry several tokens). `tokens` minus this (when the
    /// provider reports usage) is the hidden reasoning token count.
    pub visible_tokens: u64,
}

/// A gap between consecutive tokens longer than this (ms) counts as a
/// stutter — a visible streaming hiccup.
pub const STUTTER_MS: f64 = 250.0;

impl RunMetrics {
    /// Whether the accumulated tool-call arguments parsed as valid JSON
    /// containing the expected field.
    #[must_use]
    pub fn tool_args_valid(&self) -> bool {
        crate::tools::validate_args(&self.tool_args)
    }
}

impl RunMetrics {
    /// Generation window: total time minus time-to-first-byte.
    #[must_use]
    pub fn gen_ms(&self) -> f64 {
        (self.total_ms - self.ttfb_ms).max(0.0)
    }

    /// Effective generation window used for throughput: the observed
    /// streaming window, widened to the provider-reported server processing
    /// time when that is longer.
    ///
    /// Reasoning models (gpt-5, o-series) think before the first visible
    /// token and then deliver the whole completion in one burst — the
    /// observed window shrinks to milliseconds while the server spent far
    /// longer generating. Using the server-reported time keeps tokens/s
    /// an honest *generation* rate instead of a burst artifact (#125).
    #[must_use]
    pub fn effective_gen_ms(&self) -> f64 {
        self.gen_ms().max(self.server_ms.unwrap_or(0.0))
    }

    /// Single-run throughput over the effective generation window.
    #[must_use]
    pub fn tokens_per_sec(&self) -> f64 {
        if self.tokens == 0 || self.effective_gen_ms() <= 0.0 {
            0.0
        } else {
            self.tokens as f64 / (self.effective_gen_ms() / 1000.0)
        }
    }
}

/// Tuning parameters of one benchmark lane.
#[derive(Debug, Clone)]
pub struct BenchConfig {
    /// Prompt sent to every provider.
    pub prompt: String,
    /// Sampling cap sent as `max_tokens`.
    pub max_tokens: u64,
    /// Number of measured rounds per provider.
    pub runs: usize,
    /// Warmup rounds excluded from statistics.
    pub warmup: usize,
    /// Concurrent requests per provider within one round.
    ///
    /// With `concurrency > 1` the aggregate tokens/s is computed over the
    /// summed generation windows of all requests in a round, measuring
    /// saturation throughput instead of single-stream speed.
    pub concurrency: usize,
    /// Per-request timeout in seconds.
    pub timeout_secs: u64,
    /// Enable the tool-calling task (attaches tool schemas to requests).
    pub tools: bool,
    /// Which tool task to race (see `crate::tools::registry`).
    pub tool_task: String,
    /// Prompt variant label when running a sweep.
    pub variant: Option<Arc<str>>,
    /// When set, measured rounds continue until this many seconds have
    /// elapsed (overrides `runs` as the stopping condition).
    pub duration_secs: Option<u64>,
    /// Additional attempts made for a failed round before it counts as
    /// failed. `0` keeps the previous behaviour of failing immediately.
    /// Only retryable failures (timeouts, 429, 5xx) consume an attempt;
    /// permanent 4xx errors fail the round immediately.
    pub retries: usize,
    /// Sampling controls forwarded to every request in the lane.
    pub sampling: crate::provider::Sampling,
    /// Prompts drawn one per measured round, in order, shared by every
    /// lane. Empty means every round uses [`Self::prompt`].
    pub workload: Vec<crate::workload::WorkloadPrompt>,
}

impl Default for BenchConfig {
    fn default() -> Self {
        BenchConfig {
            prompt: String::new(),
            max_tokens: 256,
            runs: 3,
            warmup: 1,
            concurrency: 1,
            timeout_secs: 120,
            tools: false,
            tool_task: "weather".to_string(),
            variant: None,
            duration_secs: None,
            retries: 0,
            sampling: crate::provider::Sampling::default(),
            workload: Vec::new(),
        }
    }
}

/// Live progress notification emitted while a lane runs.
#[derive(Debug, Clone)]
pub struct LaneUpdate {
    /// Provider id (shared, cheap to clone).
    pub provider: Arc<str>,
    /// Variant label when running a sweep (shared, cheap to clone).
    pub variant: Option<Arc<str>>,
    /// Model of the lane that emitted this update; used to disambiguate
    /// multiple lanes of the same provider racing different models.
    pub model: Option<Arc<str>>,
    /// Tokens received so far in the current request.
    pub tokens: u64,
    /// Elapsed milliseconds of the current request.
    pub elapsed_ms: f64,
    /// Time-to-first-byte once observed.
    pub ttfb_ms: Option<f64>,
    /// Whether the current round just finished.
    pub finished: bool,
    /// Round or lane error message.
    pub error: Option<String>,
    /// Provider-reported server-side processing time for the current round.
    pub server_ms: Option<f64>,
    /// The most recent streaming token text (for live preview in the TUI).
    pub last_token: Option<String>,
    /// Remaining rate-limit requests reported by the provider.
    pub ratelimit: Option<String>,
}
