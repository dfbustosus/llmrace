use crate::cost;
use crate::provider::{stream_completion, CompletionRequest, Provider, StreamEvent};
use anyhow::Result;
use futures_util::StreamExt;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Default)]
pub struct RunMetrics {
    pub total_ms: f64,
    pub ttfb_ms: f64,
    pub tokens: u64,
    pub chars: u64,
    pub prompt_tokens: u64,
}

impl RunMetrics {
    pub fn gen_ms(&self) -> f64 {
        (self.total_ms - self.ttfb_ms).max(0.0)
    }

    pub fn tokens_per_sec(&self) -> f64 {
        if self.tokens == 0 || self.gen_ms() <= 0.0 {
            0.0
        } else {
            self.tokens as f64 / (self.gen_ms() / 1000.0)
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ProviderSummary {
    pub name: String,
    pub model: String,
    pub runs: Vec<RunMetrics>,
    pub error: Option<String>,
    pub prompt_cache: String,
}

impl ProviderSummary {
    fn ok_runs(&self) -> &[RunMetrics] {
        &self.runs
    }

    pub fn mean_total_ms(&self) -> f64 {
        mean(
            &self
                .ok_runs()
                .iter()
                .map(|r| r.total_ms)
                .collect::<Vec<_>>(),
        )
    }

    pub fn median_total_ms(&self) -> f64 {
        percentile(
            &self.runs.iter().map(|r| r.total_ms).collect::<Vec<_>>(),
            50.0,
        )
    }

    pub fn p95_total_ms(&self) -> f64 {
        percentile(
            &self.runs.iter().map(|r| r.total_ms).collect::<Vec<_>>(),
            95.0,
        )
    }

    pub fn mean_ttfb_ms(&self) -> f64 {
        mean(&self.runs.iter().map(|r| r.ttfb_ms).collect::<Vec<_>>())
    }

    pub fn mean_tokens(&self) -> f64 {
        mean(
            &self
                .runs
                .iter()
                .map(|r| r.tokens as f64)
                .collect::<Vec<_>>(),
        )
    }

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

    pub fn cost_per_run_usd(&self) -> Option<f64> {
        let (pin, pout) = cost::pricing(&self.name, &self.model)?;
        Some((self.mean_tokens() * pout + self.prompt_tokens_estimate() * pin) / 1_000_000.0)
    }

    pub fn prompt_tokens_estimate(&self) -> f64 {
        cost::estimate_prompt_tokens(&self.prompt_cache)
    }

    pub fn successful(&self) -> bool {
        self.error.is_none() && !self.runs.is_empty()
    }
}

#[derive(Debug, Clone, Default)]
pub struct BenchConfig {
    pub prompt: String,
    pub max_tokens: u64,
    pub runs: usize,
    pub warmup: usize,
}

#[derive(Debug, Clone)]
pub struct LaneUpdate {
    pub provider: String,
    pub tokens: u64,
    pub elapsed_ms: f64,
    pub ttfb_ms: Option<f64>,
    pub finished: bool,
    pub error: Option<String>,
}

pub async fn run_lane(
    client: reqwest::Client,
    provider: Provider,
    model: String,
    config: BenchConfig,
    prompt_cache: String,
    mut on_update: impl FnMut(LaneUpdate) + Send + 'static,
) -> ProviderSummary {
    let mut summary = ProviderSummary {
        name: provider.name.to_string(),
        model: model.clone(),
        runs: Vec::new(),
        error: None,
        prompt_cache,
    };

    for run_idx in 0..config.warmup + config.runs {
        let counted = run_idx >= config.warmup;
        let result = single_run(
            &client,
            &provider,
            &model,
            &config,
            counted,
            |tokens, elapsed_ms, ttfb_ms| {
                on_update(LaneUpdate {
                    provider: provider.name.to_string(),
                    tokens,
                    elapsed_ms,
                    ttfb_ms,
                    finished: false,
                    error: None,
                })
            },
        )
        .await;

        match result {
            Ok(m) => {
                if counted {
                    summary.runs.push(RunMetrics {
                        total_ms: m.total_ms,
                        ttfb_ms: m.ttfb_ms,
                        tokens: m.tokens,
                        chars: m.chars,
                        prompt_tokens: m.prompt_tokens,
                    });
                }
                on_update(LaneUpdate {
                    provider: provider.name.to_string(),
                    tokens: m.tokens,
                    elapsed_ms: m.total_ms,
                    ttfb_ms: Some(m.ttfb_ms),
                    finished: true,
                    error: None,
                });
            }
            Err(e) => {
                summary.error = Some(e.to_string());
                on_update(LaneUpdate {
                    provider: provider.name.to_string(),
                    tokens: 0,
                    elapsed_ms: 0.0,
                    ttfb_ms: None,
                    finished: true,
                    error: Some(e.to_string()),
                });
                return summary;
            }
        }
    }
    summary
}

async fn single_run(
    client: &reqwest::Client,
    provider: &Provider,
    model: &str,
    config: &BenchConfig,
    _counted: bool,
    mut on_progress: impl FnMut(u64, f64, Option<f64>) + Send,
) -> Result<RunMetrics> {
    let req = CompletionRequest {
        model,
        prompt: &config.prompt,
        max_tokens: config.max_tokens,
    };
    let mut stream = stream_completion(client, provider, &req).await?;
    let start = Instant::now();
    let mut ttfb: Option<Duration> = None;
    let mut tokens = 0u64;
    let mut chars = 0u64;
    let mut usage_tokens: Option<u64> = None;

    while let Some(event) = stream.next().await {
        let event: StreamEvent = event?;
        let now = Instant::now();
        if ttfb.is_none() && (event.token.is_some() || event.completion_tokens.is_some()) {
            ttfb = Some(now - start);
        }
        if let Some(t) = &event.token {
            tokens += 1;
            chars += t.len() as u64;
            on_progress(
                tokens,
                (now - start).as_secs_f64() * 1000.0,
                ttfb.map(|d| d.as_secs_f64() * 1000.0),
            );
        }
        if let Some(n) = event.completion_tokens {
            usage_tokens = Some(n);
        }
    }

    let total = start.elapsed();
    if ttfb.is_none() {
        anyhow::bail!("empty response stream");
    }
    if let Some(n) = usage_tokens {
        tokens = n;
    }
    Ok(RunMetrics {
        total_ms: total.as_secs_f64() * 1000.0,
        ttfb_ms: ttfb.map(|d| d.as_secs_f64() * 1000.0).unwrap_or(0.0),
        tokens,
        chars,
        prompt_tokens: 0,
    })
}

pub fn mean(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    xs.iter().sum::<f64>() / xs.len() as f64
}

pub fn percentile(xs: &[f64], p: f64) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    let mut sorted = xs.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    if sorted.len() == 1 {
        return sorted[0];
    }
    let idx = (p / 100.0 * (sorted.len() - 1) as f64).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}
