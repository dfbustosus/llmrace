//! The benchmark engine: lane tasks, rounds, retries and single-request
//! streaming measurement.

use super::metrics::{
    BenchConfig, LaneUpdate, RequestAttempt, RequestOutcome, RunMetrics, STUTTER_MS,
};
use super::summary::{FailureClass, LaneSummary, RoundFailure};
use crate::error::Error;
use crate::provider::{stream_completion, CompletionRequest, ProviderSpec, StreamEvent};
use futures_util::{FutureExt, StreamExt};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

/// Full-jitter exponential backoff (the AWS "Full Jitter" pattern),
/// capped at 8s. A `Retry-After` hint wins and is itself capped at 30s
/// so a hostile header cannot stall the race. Concurrent lanes mix the
/// clock into the jitter so they do not retry in lockstep.
fn backoff(attempt: usize, retry_after: Option<Duration>) -> Duration {
    if let Some(hint) = retry_after {
        let capped = hint.min(Duration::from_secs(30));
        if !capped.is_zero() {
            return capped;
        }
    }
    let shift = attempt.saturating_sub(1).min(5);
    let ceiling = 200u64.saturating_mul(1_u64 << shift).clamp(1, 8_000);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(1);
    let mixed = nanos.wrapping_mul(0x9E37_79B1) ^ (attempt as u32).wrapping_mul(0x85EB_CA6B);
    let millis = u64::from(mixed % u32::try_from(ceiling).unwrap_or(u32::MAX)).max(1);
    Duration::from_millis(millis)
}

const MAX_ANSWER_BYTES: usize = 1024 * 1024;
const MAX_TOOL_ARGS_BYTES: usize = 1024 * 1024;
const MAX_ITL_GAPS: usize = 65_536;

fn push_capped(buf: &mut String, s: &str, max_bytes: usize) {
    if buf.len() >= max_bytes {
        return;
    }
    let remaining = max_bytes - buf.len();
    if s.len() <= remaining {
        buf.push_str(s);
    } else {
        let mut end = remaining;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        buf.push_str(&s[..end]);
    }
}

async fn sleep_stoppable(dur: Duration, stop: &std::sync::atomic::AtomicBool) -> bool {
    let end = tokio::time::Instant::now() + dur;
    let step = Duration::from_millis(100);
    loop {
        if stop.load(std::sync::atomic::Ordering::Relaxed) {
            return false;
        }
        let now = tokio::time::Instant::now();
        if now >= end {
            return true;
        }
        tokio::time::sleep(step.min(end - now)).await;
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
    spawn_lane_stoppable(
        client,
        spec,
        model,
        config,
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    )
}

/// Like [`spawn_lane`], but stops starting new rounds once `stop` is set;
/// the round in flight completes and partial results are returned — the
/// graceful-interrupt path for headless races.
pub fn spawn_lane_stoppable(
    client: reqwest::Client,
    spec: ProviderSpec,
    model: String,
    config: BenchConfig,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> (
    tokio::task::JoinHandle<LaneSummary>,
    mpsc::Receiver<LaneUpdate>,
) {
    let (tx, rx) = mpsc::channel();
    let handle = spawn_lane_with_sender(client, spec, model, config, stop, Some(tx));
    (handle, rx)
}

/// Shared lane spawner allowing callers (such as `Race::spawn` or the TUI)
/// to pass a single shared `mpsc::Sender` directly — or `None` when no live
/// progress consumer is attached — avoiding per-lane forwarder threads.
pub(crate) fn spawn_lane_with_sender(
    client: reqwest::Client,
    spec: ProviderSpec,
    model: String,
    config: BenchConfig,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    tx: Option<mpsc::Sender<LaneUpdate>>,
) -> tokio::task::JoinHandle<LaneSummary> {
    let model_label: Arc<str> = Arc::from(model.as_str());
    tokio::spawn(async move {
        let mut summary = LaneSummary {
            name: spec.id.to_string(),
            model: model.clone(),
            runs: Vec::new(),
            failures: Vec::new(),
            retry_attempts: 0,
            error: None,
            prompt: config.prompt.clone(),
            concurrency: config.concurrency,
            variant: config.variant.as_ref().map(|v| v.to_string()),
            tool_task: Some(config.tool_task.clone()),
            price_book: config.price_book.clone(),
        };
        let total_rounds = config.warmup.saturating_add(config.runs);
        let deadline = config
            .duration_secs
            .map(|secs| std::time::Instant::now() + std::time::Duration::from_secs(secs));
        let mut sent_finished = false;
        for round in 0..total_rounds {
            if stop.load(std::sync::atomic::Ordering::Relaxed) && round >= config.warmup {
                break;
            }
            let counted = round >= config.warmup;
            if let Some(dl) = deadline {
                if counted && round > config.warmup && std::time::Instant::now() >= dl {
                    break;
                }
            }
            let probe = !counted && config.warmup_mode == crate::bench::WarmupMode::ConnectionProbe;
            let (prompt_id, prompt) = if probe {
                ("", "ping")
            } else {
                crate::workload::prompt_for(&config.prompt, &config.workload, config.warmup, round)
            };
            let outcome = run_round(
                &client,
                &spec,
                &model,
                &config,
                RoundPrompt {
                    id: prompt_id,
                    text: prompt,
                    probe,
                },
                &stop,
                tx.as_ref(),
            )
            .await;
            match outcome {
                Ok(metrics) => {
                    if counted {
                        summary.retry_attempts += metrics
                            .requests
                            .iter()
                            .map(RequestOutcome::retries)
                            .sum::<u64>();
                        summary.runs.push(metrics);
                    }
                }
                Err(e) => {
                    let is_last_failure = round + 1 == total_rounds && summary.runs.is_empty();
                    if let Some(tx) = &tx {
                        let _ = tx.send(LaneUpdate {
                            provider: Arc::clone(&spec.id),
                            variant: config.variant.clone(),
                            model: Some(Arc::clone(&model_label)),
                            tokens: 0,
                            elapsed_ms: 0.0,
                            ttfb_ms: None,
                            finished: is_last_failure,
                            error: Some(e.message.clone()),
                            server_ms: None,
                            last_token: None,
                            ratelimit: None,
                        });
                        if is_last_failure {
                            sent_finished = true;
                        }
                    }
                    if counted {
                        summary.retry_attempts +=
                            e.requests.iter().map(RequestOutcome::retries).sum::<u64>();
                        summary.failures.push(e);
                    }
                }
            }
        }
        if summary.runs.is_empty() {
            summary.error = summary
                .failures
                .last()
                .map(|failure| failure.message.clone())
                .or_else(|| Some("no successful rounds".to_string()));
            if !sent_finished {
                if let Some(tx) = &tx {
                    let _ = tx.send(LaneUpdate {
                        provider: Arc::from(summary.name.as_str()),
                        variant: config.variant.clone(),
                        model: Some(Arc::clone(&model_label)),
                        tokens: 0,
                        elapsed_ms: 0.0,
                        ttfb_ms: None,
                        finished: true,
                        error: summary.error.clone(),
                        server_ms: None,
                        last_token: None,
                        ratelimit: None,
                    });
                }
            }
            return summary;
        }
        if !sent_finished {
            if let Some(tx) = &tx {
                let _ = tx.send(LaneUpdate {
                    provider: Arc::from(summary.name.as_str()),
                    variant: config.variant.clone(),
                    model: Some(Arc::clone(&model_label)),
                    tokens: summary.mean_tokens() as u64,
                    elapsed_ms: summary.mean_total_ms(),
                    ttfb_ms: Some(summary.mean_ttfb_ms()),
                    finished: true,
                    error: None,
                    server_ms: summary.median_server_ms(),
                    last_token: None,
                    ratelimit: summary
                        .runs
                        .iter()
                        .rev()
                        .find_map(|r| r.ratelimit_remaining.clone()),
                });
            }
        }
        summary
    })
}

/// What one round sends: the workload prompt, or a warmup connection probe.
struct RoundPrompt<'a> {
    id: &'a str,
    text: &'a str,
    probe: bool,
}

async fn run_round(
    client: &reqwest::Client,
    spec: &ProviderSpec,
    model: &str,
    config: &BenchConfig,
    prompt: RoundPrompt<'_>,
    stop: &Arc<std::sync::atomic::AtomicBool>,
    progress: Option<&mpsc::Sender<LaneUpdate>>,
) -> Result<RunMetrics, RoundFailure> {
    let started = Instant::now();
    let RoundPrompt {
        id: prompt_id,
        text: prompt,
        probe,
    } = prompt;
    let model_label: Arc<str> = Arc::from(model);
    let mut tasks = tokio::task::JoinSet::new();
    for index in 0..config.concurrency.max(1) {
        let item = config.workload.iter().find(|item| item.id == prompt_id);
        let mut sampling = config.sampling.clone();
        if let Some(system) = item.and_then(|item| item.system.clone()) {
            sampling.system = Some(system);
        }
        if probe {
            // A connection probe carries no system prompt, JSON output
            // constraint, or tool schemas, so warmup cannot prime the prefix
            // cache or trigger HTTP 400s on minimal "ping" prompts.
            sampling.system = None;
            sampling.json_output = None;
        }
        let job = RequestJob {
            client: client.clone(),
            spec: spec.clone(),
            model: model.to_string(),
            model_label: Arc::clone(&model_label),
            prompt: prompt.to_string(),
            prompt_id: prompt_id.to_string(),
            messages: if probe {
                Vec::new()
            } else {
                item.map(|item| item.messages.clone()).unwrap_or_default()
            },
            expect: if probe {
                None
            } else {
                item.and_then(|item| item.expect.clone())
            },
            expect_match: item.map(|item| item.expect_match).unwrap_or_default(),
            max_tokens: if probe { 16 } else { config.max_tokens },
            timeout: Duration::from_secs(config.timeout_secs),
            tools: if probe {
                false
            } else {
                config.tools || item.is_some_and(|item| item.tool.is_some())
            },
            tool_task: config.tool_task.clone(),
            tool: if probe {
                None
            } else {
                item.and_then(|item| item.tool.clone())
            },
            variant_label: config.variant.clone(),
            sampling,
        };
        let progress = progress.cloned();
        let stop = Arc::clone(stop);
        let retries = config.retries;
        tasks.spawn(async move {
            (
                index,
                request_with_retries(job, retries, &stop, progress.as_ref()).await,
            )
        });
    }

    let mut requests = Vec::new();
    while let Some(joined) = tasks.join_next().await {
        match joined {
            Ok(outcome) => requests.push(outcome),
            Err(join_err) => {
                requests.push((
                    usize::MAX,
                    RequestOutcome {
                        elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
                        attempts: vec![RequestAttempt::Failure(attempt_failure(
                            &Error::Stream(format!("request task failed: {join_err}")),
                            prompt_id,
                            started.elapsed().as_secs_f64() * 1000.0,
                            1,
                        ))],
                    },
                ));
            }
        }
    }
    let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
    requests.sort_by_key(|(index, _)| *index);
    let requests: Vec<RequestOutcome> = requests.into_iter().map(|(_, outcome)| outcome).collect();
    let all: Vec<&RunMetrics> = requests
        .iter()
        .filter_map(RequestOutcome::metrics)
        .collect();
    if all.is_empty() {
        let mut failure = requests
            .iter()
            .find_map(RequestOutcome::failure)
            .cloned()
            .unwrap_or_else(|| {
                attempt_failure(
                    &Error::Protocol("round produced no metrics".into()),
                    prompt_id,
                    0.0,
                    1,
                )
            });
        failure.elapsed_ms = elapsed_ms;
        failure.requests = requests;
        Err(failure)
    } else {
        let mut itl_gaps_ms = Vec::new();
        let mut stutters = 0u64;
        for m in &all {
            itl_gaps_ms.extend(m.itl_gaps_ms.iter().copied());
            stutters += m.stutters;
        }
        let mut metrics = RunMetrics {
            requests: Vec::new(),
            total_ms: elapsed_ms,
            ttfb_ms: requests
                .iter()
                .filter_map(|r| {
                    r.metrics()
                        .map(|m| m.ttfb_ms + (r.elapsed_ms - m.total_ms).max(0.0))
                })
                .fold(f64::MAX, f64::min),
            tokens: all.iter().map(|m| m.tokens).sum(),
            chars: all.iter().map(|m| m.chars).sum(),
            prompt_tokens: all.iter().map(|m| m.prompt_tokens).sum(),
            cache_read_tokens: all.iter().map(|m| m.cache_read_tokens).sum(),
            cache_write_tokens: all.iter().map(|m| m.cache_write_tokens).sum(),
            cache_write_1h_tokens: all.iter().map(|m| m.cache_write_1h_tokens).sum(),
            reasoning_tokens: {
                let reported: Vec<u64> = all.iter().filter_map(|m| m.reasoning_tokens).collect();
                if reported.is_empty() {
                    None
                } else {
                    Some(reported.iter().sum())
                }
            },
            reasoning_billed_separately: all.iter().any(|m| m.reasoning_billed_separately),
            tool_args: all
                .iter()
                .map(|m| m.tool_args.as_str())
                .max_by_key(|s| s.len())
                .unwrap_or("")
                .to_string(),
            server_ms: all
                .iter()
                .filter_map(|m| m.server_ms)
                .fold(None::<f64>, |acc, v| Some(acc.map_or(v, |a: f64| a.max(v)))),
            request_id: all.iter().filter_map(|m| m.request_id.clone()).next(),
            ratelimit_remaining: all
                .iter()
                .filter_map(|m| m.ratelimit_remaining.clone())
                .next(),
            itl_gaps_ms,
            stutters,
            visible_tokens: all.iter().map(|m| m.visible_tokens).sum(),
            prompt_id: prompt_id.to_string(),
            task_ok: {
                let flags: Vec<bool> = all.iter().filter_map(|m| m.task_ok).collect();
                if flags.is_empty() {
                    None
                } else {
                    Some(flags.iter().all(|ok| *ok) && all.len() == requests.len())
                }
            },
        };
        metrics.requests = requests;
        Ok(metrics)
    }
}

fn attempt_failure(error: &Error, prompt_id: &str, elapsed_ms: f64, attempts: u32) -> RoundFailure {
    RoundFailure {
        task_checked: false,
        requests: Vec::new(),
        elapsed_ms,
        class: FailureClass::of(error),
        attempts,
        message: crate::util::truncate(&error.to_string(), 300),
        prompt_id: prompt_id.to_string(),
    }
}

async fn request_with_retries(
    job: RequestJob,
    retries: usize,
    stop: &std::sync::atomic::AtomicBool,
    progress: Option<&mpsc::Sender<LaneUpdate>>,
) -> RequestOutcome {
    let started = Instant::now();
    let mut attempts = Vec::new();
    loop {
        let attempt_start = Instant::now();
        let result = std::panic::AssertUnwindSafe(single_request(job.clone(), progress))
            .catch_unwind()
            .await
            .unwrap_or_else(|_| Err(Error::Stream("request task panicked".into())));
        match result {
            Ok(metrics) => {
                attempts.push(RequestAttempt::Success(metrics));
                break;
            }
            Err(error) => {
                let retry = attempts.len() < retries
                    && error.is_retryable()
                    && !stop.load(std::sync::atomic::Ordering::Relaxed);
                let mut failure = attempt_failure(
                    &error,
                    &job.prompt_id,
                    attempt_start.elapsed().as_secs_f64() * 1000.0,
                    (attempts.len() + 1) as u32,
                );
                failure.task_checked = job.tools || job.expect.is_some();
                attempts.push(RequestAttempt::Failure(failure));
                if !retry {
                    break;
                }
                if let Some(tx) = progress {
                    let _ = tx.send(LaneUpdate {
                        provider: Arc::clone(&job.spec.id),
                        variant: job.variant_label.clone(),
                        model: Some(Arc::clone(&job.model_label)),
                        tokens: 0,
                        elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
                        ttfb_ms: None,
                        finished: false,
                        error: Some(format!(
                            "request failed, retrying ({} left): {error}",
                            retries + 1 - attempts.len()
                        )),
                        server_ms: None,
                        last_token: None,
                        ratelimit: None,
                    });
                }
                if !sleep_stoppable(backoff(attempts.len(), error.retry_after()), stop).await {
                    break;
                }
            }
        }
    }
    RequestOutcome {
        elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
        attempts,
    }
}

/// Everything one in-flight request needs, bundled so the request path
/// stays a two-argument function.
#[derive(Clone)]
struct RequestJob {
    client: reqwest::Client,
    spec: ProviderSpec,
    model: String,
    model_label: Arc<str>,
    prompt: String,
    prompt_id: String,
    messages: Vec<crate::workload::WorkloadMessage>,
    expect: Option<String>,
    expect_match: crate::workload::ExpectMatch,
    max_tokens: u64,
    timeout: Duration,
    tools: bool,
    tool_task: String,
    tool: Option<crate::tools::ToolDefinition>,
    variant_label: Option<Arc<str>>,
    sampling: crate::provider::Sampling,
}

/// Streams one completion, measuring TTFB, token gaps and usage.
async fn single_request(
    job: RequestJob,
    progress: Option<&mpsc::Sender<LaneUpdate>>,
) -> crate::error::Result<RunMetrics> {
    let RequestJob {
        client,
        spec,
        model,
        model_label,
        prompt,
        prompt_id,
        messages,
        expect,
        expect_match,
        max_tokens,
        timeout,
        tools,
        tool_task,
        tool,
        variant_label,
        sampling,
    } = job;
    let req = CompletionRequest {
        model: &model,
        prompt: &prompt,
        max_tokens,
        tools,
        tool_task: Some(&tool_task),
        sampling,
        messages: &messages,
        tool: tool.as_ref(),
    };
    let start = Instant::now();
    let deadline = tokio::time::Instant::now() + timeout;
    let mut stream = tokio::time::timeout_at(deadline, stream_completion(&client, &spec, &req))
        .await
        .map_err(|_| {
            Error::Stream(format!(
                "{} ({model}): request timed out after {timeout:?}",
                spec.id
            ))
        })??;
    let server_timing = stream.server_timing();
    let mut ttfb: Option<Duration> = None;
    let mut tokens = 0u64;
    let mut chars = 0u64;
    let mut answer = String::new();
    let mut usage: Option<u64> = None;
    let mut prompt_tokens: Option<u64> = None;
    let mut cache_read_tokens = 0u64;
    let mut cache_write_tokens = 0u64;
    let mut cache_write_1h_tokens = 0u64;
    let mut reasoning_tokens: Option<u64> = None;
    let mut reasoning_billed_separately = false;
    let mut tool_args = String::new();
    let mut last_token_at: Option<Instant> = None;
    let mut itl_gaps_ms: Vec<f64> = Vec::new();
    let mut stutters = 0u64;
    let mut arg_fragments: u64 = 0;

    let sleep = tokio::time::sleep_until(deadline);
    tokio::pin!(sleep);
    loop {
        let next = tokio::select! {
            biased;
            _ = &mut sleep => {
                return Err(Error::Stream(format!(
                    "{} ({model}): request timed out after {timeout:?} ({} tokens received)",
                    spec.id,
                    tokens,
                )));
            }
            item = stream.next() => item,
        };
        let Some(event) = next else { break };
        let event: StreamEvent = event?;
        let now = Instant::now();
        let first_output = event.token.is_some() || event.tool_args.is_some();
        if ttfb.is_none() && first_output {
            ttfb = Some(now - start);
        }
        if let Some(fragment) = &event.tool_args {
            push_capped(&mut tool_args, fragment, MAX_TOOL_ARGS_BYTES);
            // streamed argument fragments are delivered content: each
            // fragment is roughly one token on OpenAI-style streams, and
            // the characters back the estimate — without this, classic
            // models in `--tools` mode appear to have hidden "reasoning"
            arg_fragments += 1;
            chars += fragment.len() as u64;
        }
        if let Some(t) = &event.token {
            if let Some(last) = last_token_at {
                let gap_ms = (now - last).as_secs_f64() * 1000.0;
                if itl_gaps_ms.len() < MAX_ITL_GAPS {
                    itl_gaps_ms.push(gap_ms);
                }
                if gap_ms > STUTTER_MS {
                    stutters += 1;
                }
            }
            last_token_at = Some(now);
            tokens += 1;
            chars += t.len() as u64;
            if expect.is_some() {
                push_capped(&mut answer, t, MAX_ANSWER_BYTES);
            }
            if let Some(tx) = progress {
                let _ = tx.send(LaneUpdate {
                    provider: Arc::clone(&spec.id),
                    variant: variant_label.clone(),
                    model: Some(Arc::clone(&model_label)),
                    tokens,
                    elapsed_ms: (now - start).as_secs_f64() * 1000.0,
                    ttfb_ms: ttfb.map(|d| d.as_secs_f64() * 1000.0),
                    finished: false,
                    error: None,
                    server_ms: None,
                    last_token: Some(t.clone()),
                    ratelimit: None,
                });
            }
        }
        if let Some(n) = event.completion_tokens {
            usage = Some(n);
        }
        // A usage-only event must not start the TTFB clock. Input tokens
        // often arrive in `message_start`, before any visible text. Cache
        // counts update independently so a later output-only usage event
        // does not wipe a cache hit reported at the start.
        if let Some(n) = event.prompt_tokens.filter(|n| *n > 0) {
            prompt_tokens = Some(n);
        }
        if let Some(n) = event.cache_read_tokens {
            cache_read_tokens = n;
        }
        if let Some(n) = event.cache_write_tokens {
            cache_write_tokens = n;
        }
        if let Some(n) = event.cache_write_1h_tokens {
            cache_write_1h_tokens = n;
        }
        if let Some(n) = event.reasoning_tokens {
            reasoning_tokens = Some(n);
            reasoning_billed_separately |= event.reasoning_billed_separately;
        }
    }

    let total = start.elapsed();
    let ttfb =
        ttfb.ok_or_else(|| Error::Protocol(format!("{}: empty response stream", spec.id)))?;
    // Visible streamed tokens: event count floored by a chars/4 estimate,
    // so coalesced multi-token deltas are not undercounted (#125).
    let visible_tokens = (tokens + arg_fragments).max((chars / 4).max(1));
    if let Some(n) = usage {
        tokens = n;
    } else {
        tokens = visible_tokens;
    }
    let task_ok = crate::tools::task_ok_with_tool(
        tools,
        &tool_task,
        &tool_args,
        tool.as_ref().map(|tool| &tool.parameters),
        expect.as_deref(),
        expect_match,
        &answer,
    );
    Ok(RunMetrics {
        requests: Vec::new(),
        total_ms: total.as_secs_f64() * 1000.0,
        ttfb_ms: ttfb.as_secs_f64() * 1000.0,
        tokens,
        chars,
        prompt_tokens: prompt_tokens.unwrap_or(0),
        cache_read_tokens,
        cache_write_tokens,
        cache_write_1h_tokens,
        reasoning_tokens,
        reasoning_billed_separately,
        tool_args,
        server_ms: server_timing.processing_ms,
        request_id: server_timing.request_id,
        ratelimit_remaining: server_timing.ratelimit_remaining,
        itl_gaps_ms,
        stutters,
        visible_tokens,
        prompt_id,
        task_ok,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_after_is_honored_and_capped() {
        assert_eq!(
            backoff(1, Some(Duration::from_secs(2))),
            Duration::from_secs(2)
        );
        assert_eq!(
            backoff(3, Some(Duration::from_secs(120))),
            Duration::from_secs(30)
        );
    }

    #[test]
    fn jitter_stays_inside_the_exponential_ceiling() {
        let first = backoff(1, None);
        assert!(first >= Duration::from_millis(1));
        assert!(first <= Duration::from_millis(200), "{first:?}");
        let later = backoff(4, None);
        assert!(later <= Duration::from_millis(1_600), "{later:?}");
        let capped = backoff(12, None);
        assert!(capped <= Duration::from_millis(8_000), "{capped:?}");
    }

    #[test]
    fn push_capped_respects_utf8_boundaries_and_byte_cap() {
        let mut buf = String::from("abc");
        push_capped(&mut buf, "ééé", 6); // 'é' is 2 bytes; remaining is 3 bytes -> fits 1 'é' (2 bytes)
        assert_eq!(buf, "abcé");
        push_capped(&mut buf, "xyz", 5);
        assert_eq!(buf, "abcé");
    }

    #[tokio::test]
    async fn spawn_lane_stoppable_always_emits_finished_update_on_early_stop() {
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let (handle, rx) = spawn_lane_stoppable(
            reqwest::Client::new(),
            ProviderSpec::mock(),
            "mock-1".into(),
            BenchConfig {
                prompt: "hi".into(),
                runs: 3,
                warmup: 0,
                ..BenchConfig::default()
            },
            stop,
        );
        let summary = handle.await.expect("lane task");
        assert!(summary.runs.is_empty());
        let updates: Vec<LaneUpdate> = rx.try_iter().collect();
        assert!(
            updates.iter().any(|u| u.finished),
            "must emit a final finished:true update: {updates:?}"
        );
    }

    #[tokio::test]
    async fn sleep_stoppable_exits_promptly_when_stopped() {
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let started = Instant::now();
        let completed = sleep_stoppable(Duration::from_secs(30), &stop).await;
        assert!(!completed);
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
