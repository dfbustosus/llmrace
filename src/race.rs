//! A high-level multi-provider race for library callers.
//!
//! The CLI is a thin wrapper over this module: resolve providers, run every
//! lane concurrently, score, render, gate. Embedding a race in a
//! `#[tokio::test]` does not need the binary's orchestration:
//!
//! ```
//! use llmrace::provider::ProviderSpec;
//! use llmrace::race::{GatePolicy, Race};
//!
//! # fn main() {
//! # let rt = tokio::runtime::Runtime::new().unwrap();
//! # rt.block_on(async {
//! let outcome = Race::new([ProviderSpec::mock(), ProviderSpec::mock()])
//!     .runs(2)
//!     .warmup(0)
//!     .run()
//!     .await
//!     .unwrap();
//! assert_eq!(outcome.summaries.len(), 2);
//! assert!(outcome.markdown().contains("llmrace report"));
//! let baseline = llmrace::regression::BaselineReport::from_json_str(&outcome.json()).unwrap();
//! let policy = GatePolicy { max_regression_pct: 10000.0, require_significance: false };
//! assert!(!outcome.gate(&baseline, policy).gate_failed(false));
//! # });
//! # }
//! ```

use crate::bench::{spawn_lane, BenchConfig, LaneSummary};
use crate::error::{Error, Result};
use crate::provider::{benchmark_client, split_model_override, ProviderSpec, Sampling};
use crate::regression::{BaselineReport, RegressionReport};
use crate::score::Weights;
use crate::workload::WorkloadPrompt;
use std::time::Duration;

/// How strictly [`RaceOutcome::gate`] fails.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GatePolicy {
    /// Regression threshold in percent.
    pub max_regression_pct: f64,
    /// Latency and throughput breaches fail only when statistically
    /// significant (Welch p < 0.05 on per-run data).
    pub require_significance: bool,
}

impl GatePolicy {
    /// The CI default: 15% and significance required.
    #[must_use]
    pub fn ci_default() -> Self {
        Self {
            max_regression_pct: 15.0,
            require_significance: true,
        }
    }
}

/// A multi-lane race, configured with builder methods and run once.
///
/// Lanes race concurrently and rounds stay sequential within a lane — the
/// same model the CLI uses.
#[derive(Debug, Clone)]
pub struct Race {
    providers: Vec<ProviderSpec>,
    bench: BenchConfig,
    models: Vec<String>,
    weights: Weights,
    winner: bool,
    region: Option<String>,
}

impl Race {
    /// A race of these providers. [`ProviderSpec::mock`] needs no network.
    #[must_use]
    pub fn new(providers: impl IntoIterator<Item = ProviderSpec>) -> Self {
        Self {
            providers: providers.into_iter().collect(),
            bench: BenchConfig::default(),
            models: Vec::new(),
            weights: Weights::default(),
            winner: false,
            region: None,
        }
    }

    /// The single prompt sent every round. Ignored when a workload is set.
    #[must_use]
    pub fn prompt(mut self, prompt: impl Into<String>) -> Self {
        self.bench.prompt = prompt.into();
        self
    }

    /// A shared workload drawn one prompt per measured round, in order.
    #[must_use]
    pub fn workload(mut self, workload: Vec<WorkloadPrompt>) -> Self {
        if let Some(first) = workload.first() {
            self.bench.prompt.clone_from(&first.prompt);
        }
        self.bench.workload = workload;
        self
    }

    /// Measured rounds per lane.
    #[must_use]
    pub fn runs(mut self, runs: usize) -> Self {
        self.bench.runs = runs.max(1);
        self
    }

    /// Warmup rounds excluded from statistics.
    #[must_use]
    pub fn warmup(mut self, warmup: usize) -> Self {
        self.bench.warmup = warmup;
        self
    }

    /// Concurrent requests per lane per round.
    #[must_use]
    pub fn concurrency(mut self, concurrency: usize) -> Self {
        self.bench.concurrency = concurrency.max(1);
        self
    }

    /// Sampling cap sent as `max_tokens`.
    #[must_use]
    pub fn max_tokens(mut self, max_tokens: u64) -> Self {
        self.bench.max_tokens = max_tokens;
        self
    }

    /// Per-request timeout in seconds.
    #[must_use]
    pub fn timeout(mut self, secs: u64) -> Self {
        self.bench.timeout_secs = secs;
        self
    }

    /// Additional attempts for a retryable failure before it counts.
    #[must_use]
    pub fn retries(mut self, retries: usize) -> Self {
        self.bench.retries = retries;
        self
    }

    /// Full sampling controls (temperature, top-p, seed, system prompt).
    #[must_use]
    pub fn sampling(mut self, sampling: Sampling) -> Self {
        self.bench.sampling = sampling;
        self
    }

    /// Fills whichever of temperature, top-p and seed is still unset with
    /// 0, 1 and 0 — the CLI's `--reproducible`.
    #[must_use]
    pub fn reproducible(mut self) -> Self {
        self.bench.sampling.temperature.get_or_insert(0.0);
        self.bench.sampling.top_p.get_or_insert(1.0);
        self.bench.sampling.seed.get_or_insert(0);
        self
    }

    /// A built-in tool task (`weather`, `math`, `lookup`).
    #[must_use]
    pub fn tools(mut self, task: impl Into<String>) -> Self {
        self.bench.tools = true;
        self.bench.tool_task = task.into();
        if let Some(task) = crate::tools::find(&self.bench.tool_task) {
            self.bench.prompt = task.prompt.to_string();
        }
        self
    }

    /// Model overrides (`"model"` for every lane, `"provider=model"` for one).
    #[must_use]
    pub fn models(mut self, models: &[&str]) -> Self {
        self.models = models.iter().map(|m| (*m).to_string()).collect();
        self
    }

    /// Scoped price overrides for this race.
    #[must_use]
    pub fn price_book(mut self, book: crate::cost::PriceBook) -> Self {
        self.bench.price_book = Some(book);
        self
    }

    /// Region tag stamped into the JSON report.
    #[must_use]
    pub fn region(mut self, region: impl Into<String>) -> Self {
        self.region = Some(region.into());
        self
    }

    /// Enables podium scoring with these weights.
    #[must_use]
    pub fn winner(mut self, weights: Weights) -> Self {
        self.winner = true;
        self.weights = weights;
        self
    }

    /// Runs every lane concurrently and collects the summaries.
    ///
    /// A lane that panics or cannot start becomes a lane error; the other
    /// lanes still finish.
    pub async fn run(self) -> Result<RaceOutcome> {
        let client = benchmark_client(
            Duration::from_secs(self.bench.timeout_secs),
            self.bench.concurrency,
        )?;
        let mut handles = Vec::with_capacity(self.providers.len());
        let mut receivers = Vec::with_capacity(self.providers.len());
        for spec in &self.providers {
            let model = resolve_model(&self.models, &spec.id, &spec.default_model);
            let (handle, rx) = spawn_lane(client.clone(), spec.clone(), model, self.bench.clone());
            handles.push(handle);
            receivers.push(rx);
        }
        drop(receivers);
        let mut summaries = Vec::with_capacity(handles.len());
        for handle in handles {
            summaries.push(
                handle
                    .await
                    .map_err(|e| Error::Stream(format!("lane task failed: {e}")))?,
            );
        }
        let request = crate::report::request_sha256(
            &self.bench.prompt,
            &self.bench.workload,
            &self.bench.sampling,
            self.bench.max_tokens,
            self.bench.tools.then_some(self.bench.tool_task.as_str()),
        );
        Ok(RaceOutcome {
            summaries,
            request_sha256: request,
            bench: self.bench,
            weights: self.weights,
            winner: self.winner,
            region: self.region,
        })
    }
}

/// The collected result of one [`Race`].
#[derive(Debug, Clone)]
pub struct RaceOutcome {
    /// Lane summaries, one per provider.
    pub summaries: Vec<LaneSummary>,
    /// SHA-256 of the request every lane sent.
    pub request_sha256: String,
    bench: BenchConfig,
    weights: Weights,
    winner: bool,
    region: Option<String>,
}

impl RaceOutcome {
    /// Measured rounds in the longest lane, or the configured count.
    #[must_use]
    pub fn actual_runs(&self) -> usize {
        self.summaries
            .iter()
            .map(|s| s.runs.len())
            .max()
            .unwrap_or(self.bench.runs)
    }

    /// Podium markdown, as the CLI prints with `--winner`. Empty when the
    /// race was not built with [`Race::winner`] or had a single lane.
    #[must_use]
    pub fn podium(&self) -> String {
        if !self.winner {
            return String::new();
        }
        let scored = crate::score::score_lanes(&self.summaries, &self.weights);
        crate::score::podium(&scored, &self.weights)
    }

    /// The markdown report the CLI writes, podium included when scored.
    #[must_use]
    pub fn markdown(&self) -> String {
        let mut md = crate::report::markdown(
            &self.summaries,
            &self.bench.prompt,
            self.actual_runs(),
            self.bench.max_tokens,
        );
        if let Some(note) = crate::report::sampling_note(&self.bench.sampling) {
            if let Some(pos) = md.find("- **Generated**:") {
                md.insert_str(pos, &note);
            } else {
                md.push_str(&note);
            }
        }
        if let Some(note) = crate::report::cache_note(&self.summaries) {
            if let Some(pos) = md.find("- **Generated**:") {
                md.insert_str(pos, &note);
            } else {
                md.push_str(&note);
            }
        }
        let podium = self.podium();
        if !podium.is_empty() {
            md.push_str("\n## Podium\n");
            md.push_str(&podium);
        }
        md
    }

    /// The JUnit XML report the CLI writes with `--junit`.
    #[must_use]
    pub fn junit(&self) -> String {
        crate::report::junit_xml(&self.summaries)
    }

    /// The JSON report the CLI writes with `--json`.
    #[must_use]
    pub fn json(&self) -> String {
        crate::report::insert_sampling_json(
            &crate::report::json_report_cited(
                &self.summaries,
                &self.bench.prompt,
                self.actual_runs(),
                self.bench.max_tokens,
                self.region.as_deref(),
                &crate::report::SentPrompts {
                    prompt: &self.bench.prompt,
                    workload: &self.bench.workload,
                    warmup: self.bench.warmup,
                },
                &self.request_sha256,
            ),
            &self.bench.sampling,
        )
    }

    /// Compares this race against a baseline report.
    ///
    /// `gate_failed(policy.require_significance)` on the returned report is
    /// the process exit code the CLI would produce for the same race.
    #[must_use]
    pub fn gate(&self, baseline: &BaselineReport, policy: GatePolicy) -> RegressionReport {
        crate::regression::evaluate_request(
            &self.summaries,
            baseline,
            policy.max_regression_pct,
            Some(&self.request_sha256),
        )
    }
}

/// Resolves the model for one provider from `model` / `provider=model`
/// overrides — the same rule the CLI applies to `--model`.
#[must_use]
pub fn resolve_model(overrides: &[String], provider_id: &str, default_model: &str) -> String {
    let mut global: Option<&str> = None;
    for entry in overrides {
        if let Some((for_provider, model)) = split_model_override(entry) {
            if for_provider == provider_id {
                return model;
            }
        } else if global.is_none() {
            global = Some(entry.as_str());
        }
    }
    global
        .map(str::to_string)
        .unwrap_or_else(|| default_model.to_string())
}
