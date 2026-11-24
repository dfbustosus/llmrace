//! Regression detection against a previously recorded baseline report.
//!
//! Enables `llmrace` to act as a CI quality gate: race the providers, then
//! compare the fresh results against a JSON report from an earlier run and
//! fail (non-zero exit) when any metric degrades beyond a threshold.

use crate::bench::LaneSummary;
use serde::Deserialize;

/// Threshold-aware comparison result for one provider.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderComparison {
    /// Provider id.
    pub name: String,
    /// Model recorded in the baseline.
    pub baseline_model: String,
    /// Model benchmarked now.
    pub current_model: String,
    /// Per-metric comparisons.
    pub metrics: Vec<MetricComparison>,
}

/// Comparison of a single metric between baseline and current run.
#[derive(Debug, Clone, PartialEq)]
pub struct MetricComparison {
    /// Human-readable metric label.
    pub label: &'static str,
    /// Baseline value.
    pub baseline: f64,
    /// Current value.
    pub current: f64,
    /// Relative change in percent (positive = worse for latency metrics).
    pub delta_pct: f64,
    /// Whether this metric regressed beyond the threshold.
    pub regressed: bool,
    /// Welch p-value for the difference, when both sides carry per-run
    /// samples (`None` for reports saved without per-run data).
    pub p_value: Option<f64>,
    /// Workload prompt this metric belongs to. `None` for a lane metric.
    pub prompt_id: Option<String>,
}

impl MetricComparison {
    /// Whether this metric fails the process, under the significance rule.
    fn fails_the_gate(&self, require_significance: bool) -> bool {
        if !self.regressed {
            return false;
        }
        if !require_significance {
            return true;
        }
        match self.p_value {
            Some(p) => p < 0.05,
            // Failure rate, goodput, cost and task success are ratios of the
            // whole sample. A threshold breach is the gate; there is no t-test.
            None => {
                matches!(
                    self.label,
                    "failure rate" | "goodput" | "cost/run" | "task success"
                )
            }
        }
    }
}

/// Outcome of a full baseline comparison.
#[derive(Debug, Clone, Default)]
pub struct RegressionReport {
    /// Per-provider comparisons for providers present in both runs.
    pub comparisons: Vec<ProviderComparison>,
    /// Providers benchmarked now that were absent from the baseline.
    pub new_providers: Vec<String>,
    /// Providers present in the baseline that were not benchmarked now.
    pub skipped_providers: Vec<String>,
    /// `true` when both sides named a request and the hashes differ.
    /// Metric comparisons are skipped: a different request is not a regression.
    pub request_mismatch: bool,
}

impl RegressionReport {
    /// `true` when at least one metric regressed beyond the threshold.
    #[must_use]
    pub fn any_regressed(&self) -> bool {
        self.comparisons
            .iter()
            .any(|c| c.metrics.iter().any(|m| m.regressed))
    }

    /// `true` when at least one metric regressed beyond the threshold
    /// **and** the difference is statistically significant (Welch
    /// p < 0.05, when per-run data allows the test). Threshold breaches
    /// without a significant p-value are treated as noise — the
    /// flake-resistant gate for CI.
    #[must_use]
    pub fn any_regressed_significant(&self) -> bool {
        self.comparisons
            .iter()
            .any(|c| c.metrics.iter().any(|m| m.fails_the_gate(true)))
    }

    /// `true` when this comparison is the one the process exits on.
    ///
    /// A request mismatch fails before any metric. With
    /// `require_significance`, a latency or throughput breach fails only
    /// when it is significant. Failure rate, goodput, cost and task success
    /// have no t-test, so a threshold breach on those counts.
    #[must_use]
    pub fn gate_failed(&self, require_significance: bool) -> bool {
        self.request_mismatch
            || self.comparisons.iter().any(|c| {
                c.metrics
                    .iter()
                    .any(|m| m.fails_the_gate(require_significance))
            })
    }

    /// The `regression` object written into `race.json` when `--baseline` is set.
    ///
    /// `ok` is false exactly when [`Self::gate_failed`] is true. `failures`
    /// names each metric that caused that, including the prompt id when the
    /// metric is one prompt. A request mismatch leaves `failures` empty.
    #[must_use]
    pub fn verdict_json(
        &self,
        threshold_pct: f64,
        require_significance: bool,
    ) -> serde_json::Value {
        let mut failures = Vec::new();
        if !self.request_mismatch {
            for comparison in &self.comparisons {
                for metric in &comparison.metrics {
                    if !metric.fails_the_gate(require_significance) {
                        continue;
                    }
                    failures.push(serde_json::json!({
                        "provider": comparison.name,
                        "metric": metric.label,
                        "prompt_id": metric.prompt_id,
                        "baseline": metric.baseline,
                        "current": metric.current,
                        "delta_pct": metric.delta_pct,
                    }));
                }
            }
        }
        serde_json::json!({
            "ok": !self.gate_failed(require_significance),
            "request_mismatch": self.request_mismatch,
            "threshold_pct": threshold_pct,
            "failures": failures,
        })
    }

    /// Renders the comparison as a terminal table.
    pub fn terminal_table(&self, max_regression_pct: f64) -> String {
        let mut out = format!(
            "regression check vs baseline (threshold {max_regression_pct:.0}%)\n{:<14} {:<24} {:>18} {:>18} {:>9}\n",
            "Provider", "Metric", "Baseline", "Current", "Δ"
        );
        out.push_str(&"-".repeat(out.len()));
        out.push('\n');
        for c in &self.comparisons {
            for m in &c.metrics {
                let mark = if m.regressed { " ✗" } else { "" };
                let p_note = match m.p_value {
                    Some(p) if p < 0.05 => format!(" p={p:.3}"),
                    Some(p) => format!(" p={p:.2} n.s."),
                    None => String::new(),
                };
                let label = match &m.prompt_id {
                    Some(id) => format!("prompt {id} {}", m.label),
                    None => m.label.to_string(),
                };
                out.push_str(&format!(
                    "{:<14} {:<24} {:>16.1} \u{2192} {:<16.1} {:>+8.1}%{}{}\n",
                    c.name, label, m.baseline, m.current, m.delta_pct, p_note, mark
                ));
            }
        }
        for name in &self.new_providers {
            out.push_str(&format!("{name:<14} new provider (no baseline)\n"));
        }
        for name in &self.skipped_providers {
            out.push_str(&format!("{name:<14} in baseline but not benchmarked\n"));
        }
        if self.request_mismatch {
            out.push_str(
                "\n✗ REQUEST CHANGED: this race is not the baseline's request, so the metrics were not compared\n",
            );
        } else if self.any_regressed() {
            out.push_str("\n✗ REGRESSION DETECTED: at least one metric exceeded the threshold\n");
        } else {
            out.push_str("\n✓ no regressions beyond threshold\n");
        }
        out
    }

    /// The regression section of a markdown report.
    ///
    /// A request mismatch names the refusal and lists no metrics. Otherwise
    /// every compared metric is a row, and the closing line matches the
    /// process exit: a breach that `--require-significance` treats as noise
    /// says so.
    #[must_use]
    pub fn markdown_section(&self, max_regression_pct: f64, require_significance: bool) -> String {
        let mut out = format!("\n## Regression\n\nThreshold {max_regression_pct:.0}%.");
        if require_significance {
            out.push_str(
                " Latency and tokens/s fail only when the difference is significant (p < 0.05).",
            );
        }
        out.push_str("\n\n");
        if self.request_mismatch {
            out.push_str(
                "The request changed, so the metrics were not compared. `regression.ok` is false.\n",
            );
            return out;
        }
        out.push_str("| Provider | Metric | Prompt | Baseline | Current | Δ | p |\n");
        out.push_str("|---|---|---|---:|---:|---:|---:|\n");
        for comparison in &self.comparisons {
            for metric in &comparison.metrics {
                let prompt = metric.prompt_id.as_deref().unwrap_or("—");
                let p = match metric.p_value {
                    Some(p) => format!("{p:.3}"),
                    None => "—".to_string(),
                };
                let mark = if metric.fails_the_gate(require_significance) {
                    " ✗"
                } else {
                    ""
                };
                out.push_str(&format!(
                    "| {} | {} | {prompt} | {:.1} | {:.1} | {:+.1}%{mark} | {p} |\n",
                    comparison.name,
                    metric.label,
                    metric.baseline,
                    metric.current,
                    metric.delta_pct
                ));
            }
        }
        if self.gate_failed(require_significance) {
            out.push_str("\nRegression detected. `regression.ok` is false.\n");
        } else if require_significance && self.any_regressed() {
            out.push_str(
                "\nA threshold was breached, and no breach is statistically significant. `regression.ok` is true.\n",
            );
        } else {
            out.push_str("\nNo regression beyond the threshold. `regression.ok` is true.\n");
        }
        out
    }
}

/// The subset of a saved JSON report needed for regression checks.
#[derive(Debug, Clone, Deserialize)]
pub struct BaselineReport {
    /// Providers recorded in the baseline.
    pub providers: Vec<BaselineProvider>,
    /// Request hash of the saved race. Absent on reports written before
    /// the request id existed; the gate then compares metrics as before.
    #[serde(default)]
    pub request_sha256: Option<String>,
}

/// One per-run measurement of a baseline (the report's `runs` array).
#[derive(Debug, Clone, Deserialize, Default)]
pub struct BaselineRun {
    /// Round latency in milliseconds.
    #[serde(default)]
    pub total_ms: f64,
    /// Round time-to-first-byte in milliseconds.
    #[serde(default)]
    pub ttfb_ms: f64,
    /// Round throughput in tokens per second.
    #[serde(default)]
    pub tokens_per_sec: f64,
    /// Workload prompt id. Empty on reports written before a round named
    /// its prompt, and on a race that used one prompt.
    #[serde(default)]
    pub prompt_id: String,
}

/// One provider entry of a baseline report.
#[derive(Debug, Clone, Deserialize)]
pub struct BaselineProvider {
    /// Provider id.
    pub provider: String,
    /// Prompt-variant label for sweep baselines; `None` for plain races.
    #[serde(default)]
    pub variant: Option<String>,
    /// Model that was benchmarked.
    #[serde(default)]
    pub model: String,
    /// Mean total latency in milliseconds.
    #[serde(default)]
    pub mean_total_ms: f64,
    /// p95 total latency in milliseconds.
    #[serde(default)]
    pub p95_total_ms: f64,
    /// Mean time-to-first-byte in milliseconds.
    #[serde(default)]
    pub mean_ttfb_ms: f64,
    /// Aggregate throughput in tokens per second.
    #[serde(default)]
    pub mean_tokens_per_sec: f64,
    /// Fraction of measured rounds that failed. Absent on baselines saved
    /// before failed rounds were gated; the metric is then skipped.
    #[serde(default)]
    pub failure_rate: Option<f64>,
    /// Tokens/s times the success rate. Absent on older baselines.
    #[serde(default)]
    pub goodput_tokens_per_sec: Option<f64>,
    /// Cost of one successful round. Absent when the model had no price.
    #[serde(default)]
    pub cost_per_run_usd: Option<f64>,
    /// Fraction of measured rounds that passed the task. Absent when the
    /// baseline had no task; the metric is then skipped.
    #[serde(default)]
    pub task_pass_rate: Option<f64>,
    /// Per-prompt task rates. Absent on baselines saved before a prompt
    /// could fail the gate on its own. A prompt omitted here is skipped.
    #[serde(default)]
    pub prompts: Vec<BaselinePrompt>,
    /// Per-run measurements, when the report was saved with them.
    #[serde(default)]
    pub runs: Vec<BaselineRun>,
}

/// One workload prompt in a baseline.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct BaselinePrompt {
    /// Workload id.
    #[serde(default)]
    pub id: String,
    /// Fraction of this prompt's measured rounds that passed. Absent when
    /// the prompt had no task.
    #[serde(default)]
    pub task_pass_rate: Option<f64>,
    /// Mean total latency of this prompt, in milliseconds. Absent on
    /// baselines written before a prompt could fail the latency gate.
    #[serde(default)]
    pub mean_total_ms: Option<f64>,
    /// Mean time to first token of this prompt, in milliseconds. Absent
    /// on baselines written before a prompt could fail the latency gate.
    #[serde(default)]
    pub mean_ttfb_ms: Option<f64>,
}

impl BaselineReport {
    /// Parses a baseline from the JSON written by `llmrace --json`.
    pub fn from_json_str(raw: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(raw)
    }
}

/// Compares a fresh race against a baseline with a regression threshold in percent.
///
/// Latency metrics (mean, p95, TTFB) regress when they increase beyond the
/// threshold; throughput regresses when it decreases beyond the threshold.
/// Providers must have succeeded in the current run to be compared.
///
/// ```
/// use llmrace::regression::{evaluate, BaselineReport};
///
/// let baseline = BaselineReport::from_json_str(
///     r#"{"providers":[{"provider":"mock","model":"m","mean_total_ms":100.0,
///        "p95_total_ms":120.0,"mean_ttfb_ms":20.0,"mean_tokens_per_sec":80.0}]}"#,
/// ).unwrap();
///
/// let current = llmrace::bench::LaneSummary {
///     name: "mock".into(),
///     model: "m".into(),
///     runs: vec![llmrace::bench::RunMetrics {
///         total_ms: 105.0, ttfb_ms: 21.0, tokens: 6, chars: 24,
///         prompt_tokens: 0, tool_args: String::new(),
///         server_ms: None, request_id: None, ratelimit_remaining: None,
///         ..llmrace::bench::RunMetrics::default()
///     }],
///     failures: Vec::new(),
///     retry_attempts: 0,
///     error: None,
///     prompt: "p".into(),
///     concurrency: 1,
///     variant: None,
///     tool_task: None,
/// };
/// let report = evaluate(&[current], &baseline, 15.0);
/// assert!(!report.any_regressed());
/// ```
pub fn evaluate(
    current: &[LaneSummary],
    baseline: &BaselineReport,
    max_regression_pct: f64,
) -> RegressionReport {
    evaluate_request(current, baseline, max_regression_pct, None)
}

/// Like [`evaluate`], and refuses the comparison when both sides name a
/// request and the hashes differ. `None` keeps the previous behaviour for
/// a caller that does not know the request.
#[must_use]
pub fn evaluate_request(
    current: &[LaneSummary],
    baseline: &BaselineReport,
    max_regression_pct: f64,
    current_request: Option<&str>,
) -> RegressionReport {
    let mut current_report = baseline_from_lanes(current);
    current_report.request_sha256 = current_request.map(str::to_string);
    compare_baselines(&current_report, baseline, max_regression_pct)
}

/// Converts live lane summaries into the baseline subset used by comparisons.
fn baseline_from_lanes(lanes: &[LaneSummary]) -> BaselineReport {
    BaselineReport {
        request_sha256: None,
        providers: lanes
            .iter()
            .filter(|s| s.successful())
            .map(|s| BaselineProvider {
                provider: s.name.clone(),
                variant: s.variant.clone(),
                model: s.model.clone(),
                mean_total_ms: s.mean_total_ms(),
                p95_total_ms: s.p95_total_ms(),
                mean_ttfb_ms: s.mean_ttfb_ms(),
                mean_tokens_per_sec: s.mean_tokens_per_sec(),
                failure_rate: s.failure_rate(),
                goodput_tokens_per_sec: Some(s.goodput_tokens_per_sec()),
                cost_per_run_usd: s.cost_per_run_usd(),
                task_pass_rate: s.task_rate(),
                prompts: s
                    .prompt_rows()
                    .into_iter()
                    .map(|row| BaselinePrompt {
                        id: row.id,
                        task_pass_rate: row.task_pass.map(|(ok, n)| {
                            if n == 0 {
                                0.0
                            } else {
                                ok as f64 / n as f64
                            }
                        }),
                        mean_total_ms: Some(row.total_ms),
                        mean_ttfb_ms: Some(row.ttfb_ms),
                    })
                    .collect(),
                runs: s
                    .runs
                    .iter()
                    .map(|r| BaselineRun {
                        total_ms: r.total_ms,
                        ttfb_ms: r.ttfb_ms,
                        tokens_per_sec: r.tokens_per_sec(),
                        prompt_id: r.prompt_id.clone(),
                    })
                    .collect(),
            })
            .collect(),
    }
}

/// Finds the baseline entry for one lane: exact variant match first, then a
/// variant-less entry for that provider (plain baselines still gate sweep
/// runs, per lane).
fn find_base<'a>(
    baseline: &'a BaselineReport,
    provider: &str,
    variant: Option<&str>,
) -> Option<&'a BaselineProvider> {
    if let Some(v) = variant {
        if let Some(hit) = baseline
            .providers
            .iter()
            .find(|p| p.provider == provider && p.variant.as_deref() == Some(v))
        {
            return Some(hit);
        }
    }
    baseline
        .providers
        .iter()
        .find(|p| p.provider == provider && p.variant.is_none())
}

/// Compares two saved JSON reports (A/B mode): `b` is treated as the
/// "current" side and `a` as the baseline.
///
/// ```
/// use llmrace::regression::{compare_baselines, BaselineReport};
///
/// let a = BaselineReport::from_json_str(
///     r#"{"providers":[{"provider":"mock","mean_total_ms":100.0,"p95_total_ms":120.0,
///        "mean_ttfb_ms":20.0,"mean_tokens_per_sec":80.0}]}"#).unwrap();
/// let b = BaselineReport::from_json_str(
///     r#"{"providers":[{"provider":"mock","mean_total_ms":200.0,"p95_total_ms":240.0,
///        "mean_ttfb_ms":40.0,"mean_tokens_per_sec":60.0}]}"#).unwrap();
/// let report = compare_baselines(&b, &a, 15.0);
/// assert!(report.any_regressed());
/// ```
pub fn compare_baselines(
    b: &BaselineReport,
    a: &BaselineReport,
    max_regression_pct: f64,
) -> RegressionReport {
    let mut report = RegressionReport::default();
    if let (Some(base), Some(current)) = (&a.request_sha256, &b.request_sha256) {
        if base != current {
            report.request_mismatch = true;
            return report;
        }
    }
    let mut compared: Vec<(String, Option<String>)> = Vec::new();

    for current in &b.providers {
        let Some(base) = find_base(a, &current.provider, current.variant.as_deref()) else {
            report.new_providers.push(current.provider.clone());
            continue;
        };
        compared.push((current.provider.clone(), current.variant.clone()));
        let mut metrics = vec![
            latency_metric(
                "mean latency",
                base.mean_total_ms,
                current.mean_total_ms,
                max_regression_pct,
                metric_p_value(&base.runs, &current.runs, |r| r.total_ms),
            ),
            latency_metric(
                "p95 latency",
                base.p95_total_ms,
                current.p95_total_ms,
                max_regression_pct,
                None, // p95 is derived, not per-run
            ),
            latency_metric(
                "ttfb",
                base.mean_ttfb_ms,
                current.mean_ttfb_ms,
                max_regression_pct,
                metric_p_value(&base.runs, &current.runs, |r| r.ttfb_ms),
            ),
            throughput_metric(
                "tokens/s",
                base.mean_tokens_per_sec,
                current.mean_tokens_per_sec,
                max_regression_pct,
                metric_p_value(&base.runs, &current.runs, |r| r.tokens_per_sec),
            ),
        ];
        // Older baselines omit these. A missing baseline value is not a
        // zero, so the gate skips the metric instead of inventing a change.
        if let (Some(base_rate), Some(current_rate)) = (base.failure_rate, current.failure_rate) {
            metrics.push(failure_rate_metric(
                base_rate,
                current_rate,
                max_regression_pct,
            ));
        }
        if let (Some(base_goodput), Some(current_goodput)) =
            (base.goodput_tokens_per_sec, current.goodput_tokens_per_sec)
        {
            metrics.push(throughput_metric(
                "goodput",
                base_goodput,
                current_goodput,
                max_regression_pct,
                None,
            ));
        }
        if let (Some(base_cost), Some(current_cost)) =
            (base.cost_per_run_usd, current.cost_per_run_usd)
        {
            metrics.push(latency_metric(
                "cost/run",
                base_cost,
                current_cost,
                max_regression_pct,
                None,
            ));
        }
        if let (Some(base_task), Some(current_task)) = (base.task_pass_rate, current.task_pass_rate)
        {
            metrics.push(task_pass_metric(
                base_task,
                current_task,
                max_regression_pct,
            ));
        }
        for base_prompt in &base.prompts {
            let Some(base_rate) = base_prompt.task_pass_rate else {
                continue;
            };
            let Some(current_rate) = current
                .prompts
                .iter()
                .find(|prompt| prompt.id == base_prompt.id)
                .and_then(|prompt| prompt.task_pass_rate)
            else {
                continue;
            };
            metrics.push(prompt_task_metric(
                &base_prompt.id,
                base_rate,
                current_rate,
                max_regression_pct,
            ));
        }
        for base_prompt in &base.prompts {
            let Some(current_prompt) = current
                .prompts
                .iter()
                .find(|prompt| prompt.id == base_prompt.id)
            else {
                continue;
            };
            let base_runs = runs_for_prompt(&base.runs, &base_prompt.id);
            let current_runs = runs_for_prompt(&current.runs, &base_prompt.id);
            if let (Some(base_ms), Some(current_ms)) =
                (base_prompt.mean_total_ms, current_prompt.mean_total_ms)
            {
                metrics.push(with_prompt(
                    latency_metric(
                        "mean latency",
                        base_ms,
                        current_ms,
                        max_regression_pct,
                        metric_p_value(&base_runs, &current_runs, |run| run.total_ms),
                    ),
                    &base_prompt.id,
                ));
            }
            if let (Some(base_ms), Some(current_ms)) =
                (base_prompt.mean_ttfb_ms, current_prompt.mean_ttfb_ms)
            {
                metrics.push(with_prompt(
                    latency_metric(
                        "ttfb",
                        base_ms,
                        current_ms,
                        max_regression_pct,
                        metric_p_value(&base_runs, &current_runs, |run| run.ttfb_ms),
                    ),
                    &base_prompt.id,
                ));
            }
        }
        report.comparisons.push(ProviderComparison {
            name: current.provider.clone(),
            baseline_model: base.model.clone(),
            current_model: current.model.clone(),
            metrics,
        });
    }

    report.skipped_providers = a
        .providers
        .iter()
        .filter(|p| !compared.contains(&(p.provider.clone(), p.variant.clone())))
        .map(|p| match &p.variant {
            Some(v) => format!("{} ({v})", p.provider),
            None => p.provider.clone(),
        })
        .collect();
    report
}

fn latency_metric(
    label: &'static str,
    baseline: f64,
    current: f64,
    threshold: f64,
    p_value: Option<f64>,
) -> MetricComparison {
    let delta_pct = if baseline.abs() < f64::EPSILON {
        0.0
    } else {
        (current - baseline) / baseline * 100.0
    };
    MetricComparison {
        label,
        baseline,
        current,
        delta_pct,
        regressed: delta_pct > threshold,
        p_value,
        prompt_id: None,
    }
}

/// Failure rate is a fraction. The delta is percentage points, so a rise
/// from 0 to 0.20 reads as +20. A zero baseline is not treated as "no
/// change": any rise past the threshold is a regression.
/// Task success is a fraction. The delta is percentage points. A fall
/// from 1.0 to 0.80 reads as -20 and regresses once it passes the threshold.
fn task_pass_metric(baseline: f64, current: f64, threshold: f64) -> MetricComparison {
    prompt_task_metric_inner(None, baseline, current, threshold)
}

fn prompt_task_metric(id: &str, baseline: f64, current: f64, threshold: f64) -> MetricComparison {
    prompt_task_metric_inner(Some(id.to_string()), baseline, current, threshold)
}

fn prompt_task_metric_inner(
    prompt_id: Option<String>,
    baseline: f64,
    current: f64,
    threshold: f64,
) -> MetricComparison {
    let points = (current - baseline) * 100.0;
    MetricComparison {
        label: "task success",
        baseline,
        current,
        delta_pct: points,
        regressed: points < -threshold,
        p_value: None,
        prompt_id,
    }
}

fn failure_rate_metric(baseline: f64, current: f64, threshold: f64) -> MetricComparison {
    let points = (current - baseline) * 100.0;
    MetricComparison {
        label: "failure rate",
        baseline,
        current,
        delta_pct: points,
        regressed: points > threshold,
        p_value: None,
        prompt_id: None,
    }
}

fn throughput_metric(
    label: &'static str,
    baseline: f64,
    current: f64,
    threshold: f64,
    p_value: Option<f64>,
) -> MetricComparison {
    let delta_pct = if baseline.abs() < f64::EPSILON {
        0.0
    } else {
        (current - baseline) / baseline * 100.0
    };
    MetricComparison {
        label,
        baseline,
        current,
        delta_pct,
        regressed: delta_pct < -threshold,
        p_value,
        prompt_id: None,
    }
}

fn with_prompt(metric: MetricComparison, id: &str) -> MetricComparison {
    MetricComparison {
        prompt_id: Some(id.to_string()),
        ..metric
    }
}

fn runs_for_prompt(runs: &[BaselineRun], id: &str) -> Vec<BaselineRun> {
    runs.iter()
        .filter(|run| run.prompt_id == id)
        .cloned()
        .collect()
}

/// Welch p-value for a metric across two per-run series, when both sides
/// carry at least two samples.
fn metric_p_value(
    base_runs: &[BaselineRun],
    current_runs: &[BaselineRun],
    pick: fn(&BaselineRun) -> f64,
) -> Option<f64> {
    if base_runs.len() < 2 || current_runs.len() < 2 {
        return None;
    }
    let a: Vec<f64> = base_runs.iter().map(pick).collect();
    let b: Vec<f64> = current_runs.iter().map(pick).collect();
    let (t, df) = crate::stats::welch_t(&a, &b)?;
    Some(crate::stats::t_two_sided_p(t, df))
}
