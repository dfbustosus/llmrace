//! The benchmark engine: lane tasks, rounds, retries and single-request
//! streaming measurement.

use super::metrics::{BenchConfig, LaneUpdate, RunMetrics, STUTTER_MS};
use super::summary::{FailureClass, LaneSummary, RoundFailure};
use crate::error::Error;
use crate::provider::{stream_completion, CompletionRequest, ProviderSpec, StreamEvent};
use futures_util::StreamExt;
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
    let model_label: Arc<str> = Arc::from(model.as_str());
    let handle = tokio::spawn(async move {
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
        let total_rounds = config.warmup + config.runs;
        let deadline = config
            .duration_secs
            .map(|secs| std::time::Instant::now() + std::time::Duration::from_secs(secs));
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
            let mut attempts_left = config.retries;
            let mut attempts_used = 0u32;
            let (prompt_id, prompt) =
                crate::workload::prompt_for(&config.prompt, &config.workload, config.warmup, round);
            let outcome = loop {
                attempts_used = attempts_used.saturating_add(1);
                let started = Instant::now();
                match run_round(&client, &spec, &model, &config, prompt_id, prompt, &tx).await {
                    Ok(metrics) => break Ok(metrics),
                    Err(e) if attempts_left > 0 && e.is_retryable() => {
                        attempts_left -= 1;
                        if counted {
                            summary.retry_attempts = summary.retry_attempts.saturating_add(1);
                        }
                        let attempt = config.retries - attempts_left;
                        tokio::time::sleep(backoff(attempt, e.retry_after())).await;
                        let _ = tx.send(LaneUpdate {
                            provider: Arc::clone(&spec.id),
                            variant: config.variant.clone(),
                            model: Some(Arc::clone(&model_label)),
                            tokens: 0,
                            elapsed_ms: 0.0,
                            ttfb_ms: None,
                            finished: false,
                            error: Some(format!(
                                "round failed, retrying ({attempts_left} left): {e}"
                            )),
                            server_ms: None,
                            last_token: None,
                            ratelimit: None,
                        });
                        continue;
                    }
                    Err(e) => {
                        if counted {
                            summary.failures.push(RoundFailure {
                                elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
                                class: FailureClass::of(&e),
                                attempts: attempts_used,
                                message: crate::util::truncate(&e.to_string(), 300),
                                prompt_id: prompt_id.to_string(),
                            });
                        }
                        break Err(e);
                    }
                }
            };
            let outcome: crate::error::Result<Vec<RunMetrics>> = outcome;
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
                        model: Some(Arc::clone(&model_label)),
                        tokens: 0,
                        elapsed_ms: 0.0,
                        ttfb_ms: None,
                        finished: round + 1 == total_rounds,
                        error: Some(e.to_string()),
                        server_ms: None,
                        last_token: None,
                        ratelimit: None,
                    });
                }
            }
        }
        if summary.runs.is_empty() {
            summary.error = summary
                .failures
                .last()
                .map(|failure| failure.message.clone())
                .or_else(|| Some("no successful rounds".to_string()));
            if summary.failures.is_empty() {
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
            return summary;
        }
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
        summary
    });
    (handle, rx)
}

async fn run_round(
    client: &reqwest::Client,
    spec: &ProviderSpec,
    model: &str,
    config: &BenchConfig,
    prompt_id: &str,
    prompt: &str,
    progress: &mpsc::Sender<LaneUpdate>,
) -> crate::error::Result<Vec<RunMetrics>> {
    let model_label: Arc<str> = Arc::from(model);
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..config.concurrency.max(1) {
        let item = config.workload.iter().find(|item| item.id == prompt_id);
        let mut sampling = config.sampling.clone();
        if let Some(system) = item.and_then(|item| item.system.clone()) {
            sampling.system = Some(system);
        }
        let job = RequestJob {
            client: client.clone(),
            spec: spec.clone(),
            model: model.to_string(),
            model_label: Arc::clone(&model_label),
            prompt: prompt.to_string(),
            prompt_id: prompt_id.to_string(),
            messages: item.map(|item| item.messages.clone()).unwrap_or_default(),
            expect: item.and_then(|item| item.expect.clone()),
            expect_match: item.map(|item| item.expect_match).unwrap_or_default(),
            max_tokens: config.max_tokens,
            timeout: Duration::from_secs(config.timeout_secs),
            tools: config.tools || item.is_some_and(|item| item.tool.is_some()),
            tool_task: config.tool_task.clone(),
            tool: item.and_then(|item| item.tool.clone()),
            variant_label: config.variant.clone(),
            sampling,
        };
        let progress = progress.clone();
        tasks.spawn(async move { single_request(job, &progress).await });
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
        let mut itl_gaps_ms = Vec::new();
        let mut stutters = 0u64;
        for m in &all {
            itl_gaps_ms.extend(m.itl_gaps_ms.iter().copied());
            stutters += m.stutters;
        }
        Ok(vec![RunMetrics {
            total_ms: all.iter().map(|m| m.total_ms).fold(f64::MIN, f64::max),
            ttfb_ms: all.iter().map(|m| m.ttfb_ms).fold(f64::MAX, f64::min),
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
                .fold(None::<f64>, |acc, v| Some(acc.map_or(v, |a: f64| a.min(v)))),
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
                    Some(flags.iter().all(|ok| *ok))
                }
            },
        }])
    }
}

/// Everything one in-flight request needs, bundled so the request path
/// stays a two-argument function.
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
    progress: &mpsc::Sender<LaneUpdate>,
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
    let mut stream = stream_completion(&client, &spec, &req).await?;
    let server_timing = stream.server_timing();
    let deadline = tokio::time::Instant::now() + timeout;
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

    loop {
        let next = tokio::select! {
            biased;
            _ = tokio::time::sleep_until(deadline) => {
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
        let first_output =
            event.token.is_some() || event.completion_tokens.is_some() || event.tool_args.is_some();
        if ttfb.is_none() && first_output {
            ttfb = Some(now - start);
        }
        if let Some(fragment) = &event.tool_args {
            tool_args.push_str(fragment);
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
                itl_gaps_ms.push(gap_ms);
                if gap_ms > STUTTER_MS {
                    stutters += 1;
                }
            }
            last_token_at = Some(now);
            tokens += 1;
            chars += t.len() as u64;
            answer.push_str(t);
            let _ = progress.send(LaneUpdate {
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
}
