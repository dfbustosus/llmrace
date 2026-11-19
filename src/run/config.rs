//! Run configuration: the plain data the race paths need, plus model
//! resolution from `--model` overrides.

use llmrace::bench::BenchConfig;
use llmrace::provider::ProviderSpec;
use llmrace::score::Weights;

/// Everything the race path needs, resolved from the CLI.
#[derive(Debug, Clone)]
pub(crate) struct RunConfig {
    /// Shared HTTP client (timeouts already configured).
    pub client: reqwest::Client,
    /// Providers racing this run.
    pub providers: Vec<ProviderSpec>,
    /// `--model` entries (`model` or `provider=model`).
    pub model_overrides: Vec<String>,
    /// Core benchmark tuning (prompt, rounds, concurrency, timeouts…).
    pub bench: BenchConfig,
    /// Prompt as typed, for report headers (differs from `bench.prompt`
    /// in `--tools` mode).
    pub prompt: String,
    /// Prompt-variant sweep spec.
    pub sweep: Option<String>,
    /// Custom prompt variants from `llmrace.toml` (name → prompt).
    pub custom_variants: std::collections::BTreeMap<String, String>,
    /// `--ramp` level spec (saturation-curve mode).
    pub ramp: Option<String>,
    /// Suppress informational output.
    pub quiet: bool,
    /// Disable the live TUI.
    pub no_tui: bool,
    /// Composite winner scoring enabled.
    pub winner: bool,
    /// Composite score weights.
    pub weights: Weights,
    /// Append the latency distribution to the markdown report.
    pub histogram: bool,
    /// Write a JSON report.
    pub json: bool,
    /// Write a CSV report.
    pub csv: bool,
    /// Write an HTML report.
    pub html: bool,
    /// Markdown report path; siblings derive from it.
    pub out: String,
    /// Region tag stamped into the JSON report.
    pub region: Option<String>,
    /// Prometheus pushgateway URL.
    pub push_gateway: Option<String>,
    /// Write Prometheus text-format metrics to this file.
    pub prom_output: Option<String>,
    /// POST watch alerts to this webhook URL.
    pub alert_webhook: Option<String>,
    /// Desktop notification on completion and alerts.
    pub notify_desktop: bool,
    /// Append every lane to the persistent history.
    pub record: bool,
    /// History backend: `jsonl` or `sqlite`.
    pub storage: String,
    /// Fail when mean TTFB exceeds this many ms.
    pub max_ttfb: Option<f64>,
    /// Baseline report for the regression gate.
    pub baseline: Option<String>,
    /// Regression threshold in percent.
    pub max_regression: f64,
    /// Only fail the regression gate on statistically significant breaches.
    pub require_significance: bool,
    /// Continuous monitoring mode.
    pub watch: bool,
    /// Seconds between watch rounds.
    pub every_secs: u64,
    /// Consecutive degraded rounds required before a --watch alert fires.
    pub alert_rounds: usize,
}

/// Resolves the model for one provider from `--model` overrides.
pub(crate) fn resolve_model(
    cli_models: &[String],
    provider_id: &str,
    default_model: &str,
) -> String {
    let mut global: Option<&str> = None;
    for entry in cli_models {
        if let Some((for_provider, model)) = llmrace::provider::split_model_override(entry) {
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
