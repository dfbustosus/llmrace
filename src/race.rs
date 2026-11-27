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
//! let policy = GatePolicy {
//!     max_regression_pct: 10000.0,
//!     require_significance: false,
//!     ..GatePolicy::default()
//! };
//! let report = outcome.gate(&baseline, policy);
//! assert!(!report.gate_failed_with_policy(policy.require_significance, policy.fail_on_inconclusive));
//! # });
//! # }
//! ```
//!
//! Dashboards and bots watch the same race live and stop it cleanly:
//!
//! ```
//! use llmrace::provider::ProviderSpec;
//! use llmrace::race::Race;
//!
//! # fn main() {
//! # let rt = tokio::runtime::Runtime::new().unwrap();
//! # rt.block_on(async {
//! let mut handle = Race::new([ProviderSpec::mock()])
//!     .runs(1)
//!     .warmup(0)
//!     .spawn()
//!     .unwrap();
//! let progress = handle.progress().unwrap();
//! let seen = std::thread::spawn(move || {
//!     let mut n = 0;
//!     while progress.recv().is_ok() {
//!         n += 1;
//!     }
//!     n
//! });
//! let outcome = handle.finish().await.unwrap();
//! assert!(seen.join().unwrap() > 0, "live updates flow before finish");
//! assert!(outcome.summaries[0].successful());
//! # });
//! # }
//! ```

use crate::bench::engine::spawn_lane_with_sender;
use crate::bench::{BenchConfig, LaneSummary, LaneUpdate, WarmupMode};
use crate::error::{Error, Result};
use crate::provider::{benchmark_client, split_model_override, ProviderSpec, Sampling};
use crate::regression::{BaselineReport, RegressionReport};
use crate::score::Weights;
use crate::workload::WorkloadPrompt;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

/// Default prompt used when callers do not supply a custom prompt or workload.
pub const DEFAULT_PROMPT: &str = "Write one vivid paragraph about a lighthouse at dawn.";

/// How strictly [`RaceOutcome::gate`] fails.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GatePolicy {
    /// Regression threshold in percent.
    pub max_regression_pct: f64,
    /// Latency and throughput breaches fail only when statistically
    /// significant (Welch p < 0.05 on per-run data).
    pub require_significance: bool,
    /// Fail the gate when a breached metric lacks a usable significance test
    /// (inconclusive evidence under [`Self::require_significance`]).
    pub fail_on_inconclusive: bool,
}

impl Default for GatePolicy {
    fn default() -> Self {
        Self {
            max_regression_pct: 15.0,
            require_significance: true,
            fail_on_inconclusive: false,
        }
    }
}

impl GatePolicy {
    /// The CI default: 15%, significance required, inconclusive breaches warn only.
    #[must_use]
    pub fn ci_default() -> Self {
        Self::default()
    }

    /// Returns `true` when `report` fails under this policy, honoring both
    /// [`Self::require_significance`] and [`Self::fail_on_inconclusive`].
    #[must_use]
    pub fn failed(&self, report: &RegressionReport) -> bool {
        report.gate_failed_with_policy(self.require_significance, self.fail_on_inconclusive)
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
            bench: BenchConfig {
                price_book: Some(crate::cost::PriceBook::new()),
                ..BenchConfig::default()
            },
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

    /// What warmup rounds send: [`WarmupMode::SamePrompt`] or
    /// [`WarmupMode::ConnectionProbe`].
    #[must_use]
    pub fn warmup_mode(mut self, mode: WarmupMode) -> Self {
        self.bench.warmup_mode = mode;
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
        self.spawn_inner(false)?.finish().await
    }

    /// Starts the race without awaiting it: live progress plus a stop
    /// handle, collected later with [`RaceHandle::finish`].
    ///
    /// Progress arrives on a plain `std::sync::mpsc` receiver, so a GUI or
    /// logger thread needs no async runtime of its own.
    pub fn spawn(self) -> Result<RaceHandle> {
        self.spawn_inner(true)
    }

    fn spawn_inner(self, with_progress: bool) -> Result<RaceHandle> {
        if self.providers.is_empty() {
            return Err(Error::Config("a race needs at least one provider".into()));
        }
        self.bench
            .sampling
            .clone()
            .checked()
            .map_err(Error::Config)?;
        if self.bench.max_tokens == 0 {
            return Err(Error::Config("max_tokens must be positive".into()));
        }
        if self.bench.warmup.checked_add(self.bench.runs).is_none() {
            return Err(Error::Config("round count overflow".into()));
        }
        let client = benchmark_client(
            Duration::from_secs(self.bench.timeout_secs),
            self.bench.concurrency,
        )?;
        let stop = Arc::new(AtomicBool::new(false));
        let (merged_tx, merged_rx) = if with_progress {
            let (tx, rx) = mpsc::channel::<LaneUpdate>();
            (Some(tx), Some(rx))
        } else {
            (None, None)
        };
        let mut handles = VecDeque::with_capacity(self.providers.len());
        for spec in &self.providers {
            let model = resolve_model(&self.models, &spec.id, &spec.default_model);
            let handle = spawn_lane_with_sender(
                client.clone(),
                spec.clone(),
                model.clone(),
                self.bench.clone(),
                Arc::clone(&stop),
                merged_tx.clone(),
            );
            handles.push_back((spec.id.to_string(), model, handle));
        }
        drop(merged_tx);
        let request = crate::report::request_sha256(
            &self.bench.prompt,
            &self.bench.workload,
            &self.bench.sampling,
            self.bench.max_tokens,
            self.bench.tools.then_some(self.bench.tool_task.as_str()),
        );
        Ok(RaceHandle {
            stop,
            progress: merged_rx,
            handles,
            bench: self.bench,
            weights: self.weights,
            winner: self.winner,
            region: self.region,
            request,
        })
    }
}

/// A running race: live per-lane progress, a stop handle, and the outcome
/// once every lane has finished or stopped.
///
/// Dropping a `RaceHandle` before [`Self::finish`] signals all lanes to stop
/// and aborts any remaining lane tasks.
pub struct RaceHandle {
    stop: Arc<AtomicBool>,
    progress: Option<mpsc::Receiver<LaneUpdate>>,
    handles: VecDeque<(String, String, tokio::task::JoinHandle<LaneSummary>)>,
    bench: BenchConfig,
    weights: Weights,
    winner: bool,
    region: Option<String>,
    request: String,
}

impl Drop for RaceHandle {
    fn drop(&mut self) {
        if !self.handles.is_empty() {
            self.stop.store(true, Ordering::Relaxed);
            for (_, _, handle) in self.handles.drain(..) {
                handle.abort();
            }
        }
    }
}

impl RaceHandle {
    /// Live per-lane progress, in arrival order. Taken once; the channel
    /// closes when the last lane finishes.
    pub fn progress(&mut self) -> Option<mpsc::Receiver<LaneUpdate>> {
        self.progress.take()
    }

    /// Signals every lane to stop after its in-flight round. Completed
    /// rounds stay in the outcome; warmup rounds always finish.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// Waits for every lane and returns the outcome.
    ///
    /// A lane task that panics or is cancelled becomes a failed [`LaneSummary`]
    /// so sibling lanes still finish and are preserved in the outcome.
    pub async fn finish(mut self) -> Result<RaceOutcome> {
        // A caller using run()/finish() without taking progress must not
        // accumulate an unread, unbounded queue of token updates.
        drop(self.progress.take());
        let mut summaries = Vec::with_capacity(self.handles.len());
        while let Some(entry) = self.handles.front_mut() {
            let res = (&mut entry.2).await;
            if let Some((name, model, _)) = self.handles.pop_front() {
                match res {
                    Ok(summary) => summaries.push(summary),
                    Err(e) => summaries.push(LaneSummary {
                        name,
                        model,
                        error: Some(format!("lane task failed: {e}")),
                        prompt: self.bench.prompt.clone(),
                        concurrency: self.bench.concurrency,
                        variant: self.bench.variant.as_ref().map(|v| v.to_string()),
                        tool_task: Some(self.bench.tool_task.clone()),
                        price_book: self.bench.price_book.clone(),
                        ..LaneSummary::default()
                    }),
                }
            }
        }
        Ok(RaceOutcome {
            summaries,
            request_sha256: std::mem::take(&mut self.request),
            bench: std::mem::take(&mut self.bench),
            weights: self.weights,
            winner: self.winner,
            region: self.region.take(),
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
            .map(|s| s.measured_rounds())
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
        let applied = crate::score::applied_weights(&self.summaries, &self.weights);
        let groups = crate::score::score_groups(&self.summaries, &applied);
        crate::score::podiums(&groups, &applied)
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
            crate::report::insert_note(&mut md, &note);
        }
        if let Some(note) = crate::report::cache_note(&self.summaries) {
            crate::report::insert_note(&mut md, &note);
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
        let json = crate::report::insert_sampling_json(
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
        );
        crate::report::insert_experiment_json(&json, &self.bench, &[])
    }

    /// Compares this race against a baseline report.
    ///
    /// Evaluating
    /// [`RegressionReport::gate_failed_with_policy`](crate::regression::RegressionReport::gate_failed_with_policy)
    /// with `(policy.require_significance, policy.fail_on_inconclusive)` (or
    /// [`GatePolicy::failed`]) on the returned report matches the process exit
    /// code the CLI produces for the same race.
    #[must_use]
    pub fn gate(&self, baseline: &BaselineReport, policy: GatePolicy) -> RegressionReport {
        crate::regression::evaluate_experiment(
            &self.summaries,
            baseline,
            policy.max_regression_pct,
            Some(&self.request_sha256),
            Some(&crate::report::experiment_sha256(&self.bench, &[])),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bench::RunMetrics;

    #[test]
    fn gate_policy_defaults_and_inconclusive_evaluation() {
        let def = GatePolicy::default();
        assert_eq!(def, GatePolicy::ci_default());
        assert!(!def.fail_on_inconclusive);
        assert!(def.require_significance);
        assert_eq!(def.max_regression_pct, 15.0);

        let inconclusive = RegressionReport {
            comparisons: vec![crate::regression::ProviderComparison {
                name: "mock".into(),
                variant: None,
                baseline_model: "m".into(),
                current_model: "m".into(),
                metrics: vec![crate::regression::MetricComparison {
                    label: "p95 latency",
                    baseline: 100.0,
                    current: 150.0,
                    delta_pct: 50.0,
                    regressed: true,
                    p_value: None,
                    prompt_id: None,
                }],
            }],
            ..RegressionReport::default()
        };
        assert!(!def.failed(&inconclusive));
        let strict = GatePolicy {
            fail_on_inconclusive: true,
            ..def
        };
        assert!(strict.failed(&inconclusive));
    }

    #[test]
    fn podium_uses_applied_weights_on_taskless_race() {
        let lane = |name: &str, ttfb: f64| LaneSummary {
            name: name.into(),
            model: "m".into(),
            runs: vec![RunMetrics {
                total_ms: 200.0,
                ttfb_ms: ttfb,
                tokens: 50,
                ..RunMetrics::default()
            }],
            concurrency: 1,
            ..LaneSummary::default()
        };
        let outcome = RaceOutcome {
            summaries: vec![lane("a", 40.0), lane("b", 80.0)],
            request_sha256: String::new(),
            bench: BenchConfig::default(),
            weights: Weights::default(),
            winner: true,
            region: None,
        };
        let podium = outcome.podium();
        assert!(podium.contains("podium"), "{podium}");
        assert!(
            !podium.contains("task 40"),
            "taskless race must not print task weight: {podium}"
        );
    }

    #[tokio::test]
    async fn finish_converts_panicked_lane_into_failed_summary() {
        let ok_lane = tokio::spawn(async {
            LaneSummary {
                name: "healthy".into(),
                model: "m1".into(),
                runs: vec![RunMetrics {
                    total_ms: 100.0,
                    ttfb_ms: 20.0,
                    tokens: 20,
                    ..RunMetrics::default()
                }],
                concurrency: 1,
                ..LaneSummary::default()
            }
        });
        let panicked_lane: tokio::task::JoinHandle<LaneSummary> =
            tokio::spawn(async { panic!("simulated lane panic") });
        let mut handles = VecDeque::new();
        handles.push_back(("panicked".to_string(), "m0".to_string(), panicked_lane));
        handles.push_back(("healthy".to_string(), "m1".to_string(), ok_lane));

        let handle = RaceHandle {
            stop: Arc::new(AtomicBool::new(false)),
            progress: None,
            handles,
            bench: BenchConfig::default(),
            weights: Weights::default(),
            winner: false,
            region: None,
            request: String::new(),
        };

        let outcome = handle
            .finish()
            .await
            .expect("finish must not short-circuit");
        assert_eq!(outcome.summaries.len(), 2);
        assert!(!outcome.summaries[0].successful());
        assert_eq!(outcome.summaries[0].name, "panicked");
        assert!(outcome.summaries[0]
            .error
            .as_deref()
            .is_some_and(|e| e.contains("lane task failed")));
        assert!(outcome.summaries[1].successful());
        assert_eq!(outcome.summaries[1].name, "healthy");
    }

    #[tokio::test]
    async fn dropping_race_handle_sets_stop_and_aborts_tasks() {
        let stop = Arc::new(AtomicBool::new(false));
        let started = Arc::new(tokio::sync::Notify::new());
        let dropped = Arc::new(AtomicBool::new(false));
        struct DropFlag(Arc<AtomicBool>);
        impl Drop for DropFlag {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let started_tx = Arc::clone(&started);
        let flag = Arc::clone(&dropped);
        let task = tokio::spawn(async move {
            let _guard = DropFlag(flag);
            started_tx.notify_one();
            tokio::time::sleep(Duration::from_secs(60)).await;
            LaneSummary::default()
        });
        started.notified().await;
        let mut handles = VecDeque::new();
        handles.push_back(("slow".to_string(), "m".to_string(), task));

        let handle = RaceHandle {
            stop: Arc::clone(&stop),
            progress: None,
            handles,
            bench: BenchConfig::default(),
            weights: Weights::default(),
            winner: false,
            region: None,
            request: String::new(),
        };
        drop(handle);
        assert!(stop.load(Ordering::Relaxed));
        tokio::task::yield_now().await;
        assert!(dropped.load(Ordering::SeqCst), "aborted task must drop");
    }

    #[test]
    fn warmup_mode_builder_sets_bench_config() {
        let race = Race::new([ProviderSpec::mock()])
            .prompt(DEFAULT_PROMPT)
            .warmup_mode(WarmupMode::ConnectionProbe);
        assert_eq!(race.bench.warmup_mode, WarmupMode::ConnectionProbe);
        assert_eq!(race.bench.prompt, DEFAULT_PROMPT);
    }
}
