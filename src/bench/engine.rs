//! The benchmark engine: lane tasks, rounds, retries and single-request
//! streaming measurement.

use super::metrics::{BenchConfig, LaneUpdate, RunMetrics, STUTTER_MS};
use super::summary::LaneSummary;
use crate::error::Error;
use crate::provider::{stream_completion, CompletionRequest, ProviderSpec, StreamEvent};
use futures_util::StreamExt;
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

/// Linear retry backoff: 500ms for the first retry, then 1s, 1.5s…
/// capped at 5s.
fn backoff(attempt: usize) -> Duration {
    Duration::from_millis((500 * attempt as u64).min(5_000))
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
    let model_label: Arc<str> = Arc::from(model.as_str());
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
            let mut attempts_left = config.retries;
            let outcome = loop {
                match run_round(&client, &spec, &model, &config, &tx).await {
                    Ok(metrics) => break Ok(metrics),
                    Err(e) if attempts_left > 0 => {
                        attempts_left -= 1;
                        // Linear backoff: an erroring endpoint (e.g. a 429)
                        // must not be hammered back-to-back.
                        let attempt = config.retries - attempts_left;
                        tokio::time::sleep(backoff(attempt)).await;
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
                    Err(e) => break Err(e),
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
                        finished: true,
                        error: Some(e.to_string()),
                        server_ms: None,
                        last_token: None,
                        ratelimit: None,
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
    progress: &mpsc::Sender<LaneUpdate>,
) -> crate::error::Result<Vec<RunMetrics>> {
    let model_label: Arc<str> = Arc::from(model);
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..config.concurrency.max(1) {
        let job = RequestJob {
            client: client.clone(),
            spec: spec.clone(),
            model: model.to_string(),
            model_label: Arc::clone(&model_label),
            prompt: config.prompt.clone(),
            max_tokens: config.max_tokens,
            timeout: Duration::from_secs(config.timeout_secs),
            tools: config.tools,
            variant_label: config.variant.clone(),
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
    max_tokens: u64,
    timeout: Duration,
    tools: bool,
    variant_label: Option<Arc<str>>,
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
        max_tokens,
        timeout,
        tools,
        variant_label,
    } = job;
    let req = CompletionRequest {
        model: &model,
        prompt: &prompt,
        max_tokens,
        tools,
    };
    let start = Instant::now();
    let mut stream = stream_completion(&client, &spec, &req).await?;
    let server_timing = stream.server_timing();
    let deadline = tokio::time::Instant::now() + timeout;
    let mut ttfb: Option<Duration> = None;
    let mut tokens = 0u64;
    let mut chars = 0u64;
    let mut usage: Option<u64> = None;
    let mut tool_args = String::new();
    let mut last_token_at: Option<Instant> = None;
    let mut itl_gaps_ms: Vec<f64> = Vec::new();
    let mut stutters = 0u64;

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
    }

    let total = start.elapsed();
    let ttfb =
        ttfb.ok_or_else(|| Error::Protocol(format!("{}: empty response stream", spec.id)))?;
    // Visible streamed tokens: event count floored by a chars/4 estimate,
    // so coalesced multi-token deltas are not undercounted (#125).
    let visible_tokens = tokens.max((chars / 4).max(1));
    if let Some(n) = usage {
        tokens = n;
    } else {
        tokens = visible_tokens;
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
        itl_gaps_ms,
        stutters,
        visible_tokens,
    })
}
