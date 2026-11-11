//! Benchmark engine: runs rounds of concurrent streaming requests per provider
//! and aggregates latency, throughput and token statistics.

use crate::error::Error;
use crate::provider::{stream_completion, CompletionRequest, ProviderSpec, StreamEvent};
use crate::stats::Sample;
use futures_util::StreamExt;
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

/// Raw measurements from a single streamed completion.
#[derive(Debug, Clone, Default)]
pub struct RunMetrics {
    /// Wall-clock time from request to last token, in milliseconds.
    pub total_ms: f64,
    /// Time from request to first token, in milliseconds.
    pub ttfb_ms: f64,
    /// Completion tokens (provider-reported usage when available).
    pub tokens: u64,
    /// Sum of token character lengths.
    pub chars: u64,
    /// Estimated prompt tokens (0 when unknown).
    pub prompt_tokens: u64,
    /// Accumulated tool-call argument JSON (empty when the run had none).
    pub tool_args: String,
    /// Provider-reported server-side processing time in milliseconds,
    /// from response headers (e.g. OpenAI's `openai-processing-ms`).
    pub server_ms: Option<f64>,
    /// Provider request ID for correlation with provider dashboards.
    pub request_id: Option<String>,
    /// Remaining rate-limit requests after this call.
    pub ratelimit_remaining: Option<String>,
}

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

    /// Single-run throughput over the generation window.
    #[must_use]
    pub fn tokens_per_sec(&self) -> f64 {
        if self.tokens == 0 || self.gen_ms() <= 0.0 {
            0.0
        } else {
            self.tokens as f64 / (self.gen_ms() / 1000.0)
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
    /// Prompt variant label when running a sweep.
    pub variant: Option<Arc<str>>,
    /// When set, measured rounds continue until this many seconds have
    /// elapsed (overrides `runs` as the stopping condition).
    pub duration_secs: Option<u64>,
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
            variant: None,
            duration_secs: None,
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
}

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

    /// Aggregate throughput: total tokens over the summed generation window.
    #[must_use]
    pub fn mean_tokens_per_sec(&self) -> f64 {
        if self.runs.is_empty() {
            return 0.0;
        }
        let total_tokens: f64 = self.runs.iter().map(|r| r.tokens as f64).sum();
        let total_gen: f64 = self.runs.iter().map(|r| r.gen_ms() / 1000.0).sum();
        if total_gen <= 0.0 {
            0.0
        } else {
            total_tokens / total_gen
        }
    }

    /// Standard percentile ladder of round total latencies:
    /// p10, p25, p50, p75, p90, p95, p99 (linear interpolation).
    #[must_use]
    pub fn latency_percentiles(&self) -> Vec<(&'static str, f64)> {
        let sample =
            crate::stats::Sample::from_values(self.runs.iter().map(|r| r.total_ms).collect());
        [
            ("p10", 10.0),
            ("p25", 25.0),
            ("p50", 50.0),
            ("p75", 75.0),
            ("p90", 90.0),
            ("p95", 95.0),
            ("p99", 99.0),
        ]
        .into_iter()
        .map(|(label, p)| (label, sample.percentile(p)))
        .collect()
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
    /// arguments; `None` when no round produced tool arguments.
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
        let valid = with_args.iter().filter(|r| r.tool_args_valid()).count();
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

/// Spawns a benchmark lane as an independent task.
///
/// Returns the task handle yielding the final [`LaneSummary`] plus a receiver
/// of live [`LaneUpdate`] progress. Warmup rounds run first, then measured
/// rounds; every round fires `concurrency` requests simultaneously. Panics in
/// a single request are contained and surface as errors, never aborting the
/// process.
pub fn spawn_lane(
    client: reqwest::Client,
    spec: ProviderSpec,
    model: String,
    config: BenchConfig,
) -> (
    tokio::task::JoinHandle<LaneSummary>,
    mpsc::Receiver<LaneUpdate>,
) {
    let (tx, rx) = mpsc::channel();
    let handle = tokio::spawn(async move {
        let mut summary = LaneSummary {
            name: spec.id.to_string(),
            model: model.clone(),
            runs: Vec::new(),
            error: None,
            prompt: config.prompt.clone(),
            concurrency: config.concurrency,
            variant: config.variant.as_ref().map(|v| v.to_string()),
        };
        let total_rounds = config.warmup + config.runs;
        let deadline = config
            .duration_secs
            .map(|secs| std::time::Instant::now() + std::time::Duration::from_secs(secs));
        for round in 0..total_rounds {
            let counted = round >= config.warmup;
            if let Some(dl) = deadline {
                if counted && round > config.warmup && std::time::Instant::now() >= dl {
                    break;
                }
            }
            let outcome = run_round(&client, &spec, &model, &config, &tx).await;
            match outcome {
                Ok(metrics) => {
                    if counted {
                        summary.runs.extend(metrics);
                    }
                }
                Err(e) => {
                    let _ = tx.send(LaneUpdate {
                        provider: Arc::clone(&spec.id),
                        variant: config.variant.clone(),
                        tokens: 0,
                        elapsed_ms: 0.0,
                        ttfb_ms: None,
                        finished: true,
                        error: Some(e.to_string()),
                        server_ms: None,
                    });
                    if summary.runs.is_empty() && round + 1 == total_rounds {
                        summary.error = Some(e.to_string());
                        return summary;
                    }
                }
            }
        }
        if summary.runs.is_empty() {
            summary.error = Some("no successful rounds".to_string());
        }
        let _ = tx.send(LaneUpdate {
            provider: Arc::from(summary.name.as_str()),
            variant: config.variant.clone(),
            tokens: summary.mean_tokens() as u64,
            elapsed_ms: summary.mean_total_ms(),
            ttfb_ms: Some(summary.mean_ttfb_ms()),
            finished: true,
            error: None,
            server_ms: summary.median_server_ms(),
        });
        summary
    });
    (handle, rx)
}

async fn run_round(
    client: &reqwest::Client,
    spec: &ProviderSpec,
    model: &str,
    config: &BenchConfig,
    progress: &mpsc::Sender<LaneUpdate>,
) -> crate::error::Result<Vec<RunMetrics>> {
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..config.concurrency.max(1) {
        let client = client.clone();
        let spec = spec.clone();
        let model = model.to_string();
        let prompt = config.prompt.clone();
        let max_tokens = config.max_tokens;
        let timeout = Duration::from_secs(config.timeout_secs);
        let progress = progress.clone();
        let tools = config.tools;
        let variant_label = config.variant.clone();
        tasks.spawn(async move {
            single_request(
                &client,
                &spec,
                &model,
                &prompt,
                max_tokens,
                timeout,
                tools,
                variant_label,
                &progress,
            )
            .await
        });
    }

    let mut all = Vec::new();
    let mut first_err: Option<Error> = None;
    while let Some(joined) = tasks.join_next().await {
        match joined {
            Ok(Ok(m)) => all.push(m),
            Ok(Err(e)) => {
                first_err.get_or_insert(e);
            }
            Err(join_err) => {
                first_err
                    .get_or_insert(Error::Stream(format!("request task panicked: {join_err}")));
            }
        }
    }
    if all.is_empty() {
        Err(first_err.unwrap_or_else(|| Error::Protocol("round produced no metrics".into())))
    } else {
        Ok(vec![RunMetrics {
            total_ms: all.iter().map(|m| m.total_ms).fold(f64::MIN, f64::max),
            ttfb_ms: all.iter().map(|m| m.ttfb_ms).fold(f64::MAX, f64::min),
            tokens: all.iter().map(|m| m.tokens).sum(),
            chars: all.iter().map(|m| m.chars).sum(),
            prompt_tokens: all.iter().map(|m| m.prompt_tokens).sum(),
            tool_args: all
                .iter()
                .map(|m| m.tool_args.as_str())
                .max_by_key(|s| s.len())
                .unwrap_or("")
                .to_string(),
            server_ms: all
                .iter()
                .filter_map(|m| m.server_ms)
                .fold(None::<f64>, |acc, v| Some(acc.map_or(v, |a: f64| a.min(v)))),
            request_id: all.iter().filter_map(|m| m.request_id.clone()).next(),
            ratelimit_remaining: all
                .iter()
                .filter_map(|m| m.ratelimit_remaining.clone())
                .next(),
        }])
    }
}

#[allow(clippy::too_many_arguments)]
async fn single_request(
    client: &reqwest::Client,
    spec: &ProviderSpec,
    model: &str,
    prompt: &str,
    max_tokens: u64,
    timeout: Duration,
    tools: bool,
    variant_label: Option<Arc<str>>,
    progress: &mpsc::Sender<LaneUpdate>,
) -> crate::error::Result<RunMetrics> {
    let req = CompletionRequest {
        model,
        prompt,
        max_tokens,
        tools,
    };
    let start = Instant::now();
    let mut stream = stream_completion(client, spec, &req).await?;
    let server_timing = stream.server_timing();
    let deadline = tokio::time::Instant::now() + timeout;
    let mut ttfb: Option<Duration> = None;
    let mut tokens = 0u64;
    let mut chars = 0u64;
    let mut usage: Option<u64> = None;
    let mut tool_args = String::new();

    loop {
        let next = tokio::select! {
            biased;
            _ = tokio::time::sleep_until(deadline) => {
                return Err(Error::Stream(format!("{}: request timed out", spec.id)));
            }
            item = stream.next() => item,
        };
        let Some(event) = next else { break };
        let event: StreamEvent = event?;
        let now = Instant::now();
        let first_output =
            event.token.is_some() || event.completion_tokens.is_some() || event.tool_args.is_some();
        if ttfb.is_none() && first_output {
            ttfb = Some(now - start);
        }
        if let Some(fragment) = &event.tool_args {
            tool_args.push_str(fragment);
        }
        if let Some(t) = &event.token {
            tokens += 1;
            chars += t.len() as u64;
            let _ = progress.send(LaneUpdate {
                provider: Arc::clone(&spec.id),
                variant: variant_label.clone(),
                tokens,
                elapsed_ms: (now - start).as_secs_f64() * 1000.0,
                ttfb_ms: ttfb.map(|d| d.as_secs_f64() * 1000.0),
                finished: false,
                error: None,
                server_ms: None,
            });
        }
        if let Some(n) = event.completion_tokens {
            usage = Some(n);
        }
    }

    let total = start.elapsed();
    let ttfb =
        ttfb.ok_or_else(|| Error::Protocol(format!("{}: empty response stream", spec.id)))?;
    if let Some(n) = usage {
        tokens = n;
    }
    Ok(RunMetrics {
        total_ms: total.as_secs_f64() * 1000.0,
        ttfb_ms: ttfb.as_secs_f64() * 1000.0,
        tokens,
        chars,
        prompt_tokens: 0,
        tool_args,
        server_ms: server_timing.processing_ms,
        request_id: server_timing.request_id,
        ratelimit_remaining: server_timing.ratelimit_remaining,
    })
}
