//! llmrace CLI: argument definitions, profile/provider resolution and
//! dispatch. Execution lives in [`run`], standalone modes in [`modes`].

#![forbid(unsafe_code)]

mod modes;
mod run;

use anyhow::{Context, Result};
use clap::Parser;
use llmrace::bench::BenchConfig;
use llmrace::provider::ProviderSpec;
use run::{resolve_model, RunConfig};
use std::sync::Arc;

#[derive(Parser)]
#[command(
    name = "llmrace",
    version,
    about = "\u{1f3c1} Race LLM providers head-to-head in your terminal",
    long_about = "llmrace benchmarks streaming latency, throughput and cost across LLM providers (OpenAI, Anthropic, Groq, OpenRouter, local Ollama or any OpenAI-compatible endpoint) with a live TUI race and shareable markdown, JSON and CSV reports."
)]
pub(crate) struct Cli {
    /// Providers to race (see --list). Omit to auto-detect from env vars.
    /// `provider=model` entries race a specific model; repeat a provider
    /// with different models to race them head-to-head.
    providers: Vec<String>,

    /// Prompt sent to every provider.
    #[arg(
        short,
        long,
        default_value = "Write one vivid paragraph about a lighthouse at dawn."
    )]
    prompt: String,

    /// Read the prompt from a file (use `-` for stdin).
    #[arg(help_heading = "Race", long)]
    prompt_file: Option<String>,

    /// Measured rounds per provider.
    #[arg(help_heading = "Race", short = 'n', long, default_value_t = 3)]
    runs: usize,

    /// Warmup rounds excluded from statistics.
    #[arg(help_heading = "Race", long, default_value_t = 1)]
    warmup: usize,

    /// Run rounds for this many seconds instead of a fixed count.
    #[arg(help_heading = "Race", long)]
    duration: Option<u64>,

    /// Concurrent requests per provider per round.
    #[arg(help_heading = "Race", short = 'c', long, default_value_t = 1)]
    concurrency: usize,

    /// Saturation curve: race at each concurrency level (e.g. 1,2,4,8) and
    /// report throughput per level; conflicts with -c, --sweep and --watch.
    #[arg(help_heading = "Race", long, conflicts_with_all = ["concurrency", "sweep", "watch"])]
    ramp: Option<String>,

    /// Model override: plain (`gpt-4o-mini`) applies to every provider,
    /// `provider=model` (e.g. `ollama=llama3.2:1b`) targets one. Repeatable.
    #[arg(help_heading = "Race", short, long)]
    model: Vec<String>,

    /// Sampling cap passed as max_tokens.
    #[arg(help_heading = "Race", long, default_value_t = 256)]
    max_tokens: u64,

    /// Per-request timeout in seconds.
    #[arg(help_heading = "Race", long, default_value_t = 120)]
    timeout: u64,

    /// Retry a failed round up to N additional times before it counts as failed.
    #[arg(help_heading = "Race", long, default_value_t = 0)]
    retries: usize,

    /// Declare an overall winner with a composite score (ttfb/tps/p95 weighted).
    #[arg(help_heading = "Scoring", long)]
    winner: bool,

    /// Composite score weights: ttfb=30,tps=40,p95=30 (with --winner).
    #[arg(help_heading = "Scoring", long)]
    weights: Option<String>,

    /// Compare two saved JSON reports side by side (a.json,b.json) and exit.
    #[arg(help_heading = "Modes", long, value_delimiter = ',')]
    compare: Vec<String>,

    /// Merge region-tagged probe reports (comma-separated) into a
    /// cross-region comparison table and exit.
    #[arg(help_heading = "Modes", long, value_delimiter = ',')]
    merge: Vec<String>,

    /// Region tag stamped into the JSON report (used by distributed probes).
    #[arg(help_heading = "Modes", long)]
    region: Option<String>,

    /// Register a custom OpenAI-compatible endpoint (e.g. vLLM, LiteLLM).
    #[arg(help_heading = "Custom endpoint", long, requires = "provider_name")]
    endpoint: Option<String>,

    /// Name for the custom endpoint provider.
    #[arg(help_heading = "Custom endpoint", long)]
    provider_name: Option<String>,

    /// Env var holding the API key for the custom endpoint.
    #[arg(help_heading = "Custom endpoint", long)]
    api_key_env: Option<String>,

    /// Disable the live TUI (CI-friendly).
    #[arg(help_heading = "Output", long)]
    no_tui: bool,

    /// Also write a JSON report.
    #[arg(help_heading = "Output", long)]
    json: bool,

    /// Also write a CSV report with per-run rows.
    #[arg(help_heading = "Output", long)]
    csv: bool,

    /// Also write a self-contained shareable HTML report.
    #[arg(help_heading = "Output", long)]
    html: bool,

    /// Compare results against a baseline JSON report (CI quality gate).
    #[arg(help_heading = "CI gates", long)]
    baseline: Option<String>,

    /// Maximum tolerated regression in percent when --baseline is set.
    #[arg(help_heading = "CI gates", long, default_value_t = 15.0)]
    max_regression: f64,

    /// With --baseline: only fail the gate on statistically significant
    /// regressions (Welch p < 0.05 on per-run data) — flake-resistant CI.
    #[arg(help_heading = "CI gates", long)]
    require_significance: bool,

    /// Exit 1 if any provider's mean TTFB exceeds this many milliseconds.
    #[arg(help_heading = "CI gates", long)]
    max_ttfb: Option<f64>,

    /// Output report path (markdown).
    #[arg(
        help_heading = "Output",
        short,
        long,
        default_value = "llmrace-report.md"
    )]
    out: String,

    /// Append this run to the persistent history (~/.local/share/llmrace/history.jsonl).
    #[arg(help_heading = "History & monitoring", long)]
    record: bool,

    /// History storage backend for --record/--trend/--dashboard.
    #[arg(help_heading = "History & monitoring", long, default_value = "jsonl")]
    storage: String,

    /// Print provider trends from recorded history and exit (with --days).
    #[arg(help_heading = "History & monitoring", long)]
    trend: bool,

    /// Render the recorded history into a static HTML dashboard and exit.
    #[arg(help_heading = "History & monitoring", long)]
    dashboard: bool,

    /// Days of history to analyze with --trend.
    #[arg(help_heading = "History & monitoring", long, default_value_t = 30)]
    days: u64,

    /// Include the latency distribution section (percentiles + histogram) in the markdown report.
    #[arg(help_heading = "Output", long)]
    histogram: bool,

    /// Benchmark tool calling and measure argument validity. Optional task
    /// name from the registry (weather, math, lookup); defaults to weather.
    /// Conflicts with --sweep (both control the prompt).
    #[arg(help_heading = "Prompt shaping", long, num_args = 0..=1, default_missing_value = "weather", conflicts_with = "sweep")]
    tools: Option<String>,

    /// Push race metrics to a Prometheus pushgateway job URL after the race.
    #[arg(help_heading = "History & monitoring", long)]
    push_gateway: Option<String>,

    /// Continuous monitoring: re-run the race every N seconds and alert on degradation.
    #[arg(help_heading = "History & monitoring", long)]
    watch: bool,

    /// Seconds between watch rounds (with --watch).
    #[arg(help_heading = "History & monitoring", long, default_value_t = 900)]
    every: u64,

    /// Consecutive degraded rounds before a --watch alert fires (debounce).
    #[arg(help_heading = "History & monitoring", long, default_value_t = 1)]
    alert_rounds: usize,

    /// Run the race once per comma-separated prompt variant (short,long,code,json,reasoning).
    #[arg(help_heading = "Prompt shaping", long)]
    sweep: Option<String>,

    /// Load a named profile from llmrace.toml (explicit flags win).
    #[arg(help_heading = "Prompt shaping", long)]
    profile: Option<String>,

    /// List built-in providers.
    #[arg(help_heading = "Modes", long)]
    list: bool,

    /// List the models a provider currently offers (e.g. `--models groq`).
    /// Bare `--models` lists models for every auto-detected provider.
    #[arg(help_heading = "Modes", long, num_args = 0..=1, default_missing_value = "*")]
    models: Option<String>,

    /// Run environment diagnostics (keys, endpoints, Ollama, config, history).
    #[arg(help_heading = "Modes", long)]
    doctor: bool,

    /// Suppress terminal output except for errors (for scripts and CI).
    #[arg(help_heading = "Output", long)]
    quiet: bool,

    /// Create a starter llmrace.toml with example profiles and exit.
    #[arg(help_heading = "Modes", long)]
    init: bool,

    /// Override pricing for a model: provider/model=input:output (USD per million tokens). Repeatable.
    #[arg(help_heading = "Prompt shaping", long = "price")]
    prices: Vec<String>,

    /// Print shell completions for the given shell and exit.
    #[arg(help_heading = "Modes", long)]
    completions: Option<String>,

    /// Print the man page (roff) and exit.
    #[arg(help_heading = "Modes", long)]
    man: bool,

    /// Print the JSON Schema for race.json reports and exit.
    #[arg(help_heading = "Modes", long)]
    schema: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Parse once, keeping the matches so profile application can tell
    // explicitly-passed flags apart from clap defaults (ValueSource) —
    // no raw `std::env::args` scanning.
    let matches = <Cli as clap::CommandFactory>::command().get_matches();
    let explicit =
        |id: &str| matches.value_source(id) == Some(clap::parser::ValueSource::CommandLine);
    let mut cli = <Cli as clap::FromArgMatches>::from_arg_matches(&matches)?;
    if let Some(source) = &cli.prompt_file {
        let content = if source == "-" {
            use std::io::Read;
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            buf
        } else {
            std::fs::read_to_string(source)
                .with_context(|| format!("reading prompt file {source}"))?
        };
        cli.prompt = content;
    }
    let (cli, config) = apply_profile(cli, &explicit)?;
    let custom_variants = config
        .as_ref()
        .map(|c| c.variants.clone())
        .unwrap_or_default();
    let code = dispatch(cli, custom_variants).await?;
    if code != 0 {
        std::process::exit(code);
    }
    Ok(())
}

/// Routes to a standalone mode or the race; returns the process exit code.
async fn dispatch(
    cli: Cli,
    custom_variants: std::collections::BTreeMap<String, String>,
) -> Result<i32> {
    if cli.init {
        return modes::init().map(|_| 0);
    }
    if cli.doctor {
        return modes::doctor(cli.json).await.map(|_| 0);
    }
    if let Some(shell) = &cli.completions {
        return modes::completions(shell).map(|_| 0);
    }
    if cli.man {
        return modes::man().map(|_| 0);
    }
    if cli.schema {
        print!("{}", llmrace::schema::race_report_schema());
        return Ok(0);
    }
    if cli.list {
        return modes::list(cli.json).map(|_| 0);
    }
    if let Some(target) = &cli.models {
        // a custom endpoint takes precedence when named explicitly or
        // when listing "everything available"
        let custom_spec = match (&cli.endpoint, &cli.provider_name) {
            (Some(endpoint), Some(name)) => {
                let model = resolve_model(&cli.model, "custom", "default");
                Some(ProviderSpec::custom(
                    name,
                    endpoint,
                    cli.api_key_env.as_deref(),
                    &model,
                ))
            }
            _ => None,
        };
        let providers = if target == "*" {
            let mut list: Vec<ProviderSpec> = ProviderSpec::registry()
                .into_iter()
                .filter(|p| *p.id != *"mock" && p.is_available() && !p.opt_in)
                .collect();
            list.extend(custom_spec);
            list
        } else if let Some(spec) = custom_spec.filter(|c| *c.id == *target) {
            vec![spec]
        } else {
            vec![ProviderSpec::find(target)
                .with_context(|| format!("unknown provider '{target}' (try --list)"))?]
        };
        if providers.is_empty() {
            anyhow::bail!("no providers available; set an API key or name one explicitly");
        }
        return modes::models(providers).await.map(|_| 0);
    }
    if cli.dashboard {
        return modes::dashboard(cli.days, &cli.out, &cli.storage).map(|_| 0);
    }
    if cli.merge.len() >= 2 {
        return modes::merge(&cli.merge, cli.json, &cli.out).map(|_| 0);
    }
    if cli.merge.len() == 1 {
        anyhow::bail!("--merge expects two or more comma-separated report paths");
    }
    if cli.compare.len() == 2 {
        return modes::compare(&cli.compare, cli.max_regression).map(|_| 0);
    }
    if cli.compare.len() == 1 {
        anyhow::bail!("--compare expects two comma-separated report paths (a.json,b.json)");
    }
    if cli.trend {
        return modes::trend(cli.days, &cli.providers, &cli.storage).map(|_| 0);
    }

    if !cli.prices.is_empty() {
        let mut parsed = Vec::new();
        for raw in &cli.prices {
            parsed.push(llmrace::cost::parse_override(raw)?);
        }
        llmrace::cost::set_overrides(parsed);
    }

    let cfg = build_run_config(cli, custom_variants)?;
    if let Some(spec) = &cfg.ramp {
        let levels = run::parse_ramp_levels(spec)?;
        run::ramp_race(cfg, &levels).await
    } else if cfg_watch(&cfg) {
        run::watch_race(cfg).await
    } else {
        run::run_race(cfg).await
    }
}

fn cfg_watch(cfg: &RunConfig) -> bool {
    cfg.watch
}

/// Resolves the CLI into the plain data the race path needs.
fn build_run_config(
    cli: Cli,
    custom_variants: std::collections::BTreeMap<String, String>,
) -> Result<RunConfig> {
    let selected = resolve_providers(&cli)?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(cli.timeout.max(1)))
        .build()
        .context("failed to build http client")?;

    let runs = if cli.duration.is_some() {
        100_000
    } else {
        cli.runs
    };
    let bench = BenchConfig {
        prompt: match &cli.tools {
            Some(task) => {
                let t = llmrace::tools::find(task).with_context(|| {
                    format!("unknown tool task '{task}' (weather, math, lookup)")
                })?;
                t.prompt.to_string()
            }
            None => cli.prompt.clone(),
        },
        max_tokens: cli.max_tokens,
        runs: runs.max(1),
        warmup: cli.warmup,
        concurrency: cli.concurrency.max(1),
        timeout_secs: cli.timeout,
        tools: cli.tools.is_some(),
        tool_task: cli.tools.clone().unwrap_or_else(|| "weather".to_string()),
        variant: None,
        duration_secs: cli.duration,
        retries: cli.retries,
    };
    let weights = match &cli.weights {
        Some(spec) => llmrace::score::Weights::parse(spec)
            .map_err(|e| anyhow::anyhow!("{e}"))
            .with_context(|| "parsing --weights")?,
        None => llmrace::score::Weights::default(),
    };
    Ok(RunConfig {
        client,
        providers: selected,
        model_overrides: cli.model,
        bench,
        prompt: cli.prompt,
        sweep: cli.sweep,
        custom_variants,
        ramp: cli.ramp,
        quiet: cli.quiet,
        no_tui: cli.no_tui,
        winner: cli.winner,
        weights,
        histogram: cli.histogram,
        json: cli.json,
        csv: cli.csv,
        html: cli.html,
        out: cli.out,
        region: cli.region,
        push_gateway: cli.push_gateway,
        record: cli.record,
        storage: cli.storage,
        max_ttfb: cli.max_ttfb,
        baseline: cli.baseline,
        max_regression: cli.max_regression,
        require_significance: cli.require_significance,
        watch: cli.watch,
        every_secs: cli.every,
        alert_rounds: cli.alert_rounds.max(1),
    })
}

#[cfg(feature = "sqlite")]
pub(crate) fn sqlite_path() -> std::path::PathBuf {
    llmrace::history::history_path().with_extension("sqlite3")
}

pub(crate) fn load_history(storage: &str) -> Result<Vec<llmrace::history::HistoryRecord>> {
    match storage {
        "jsonl" => Ok(llmrace::history::load(&llmrace::history::history_path())),
        #[cfg(feature = "sqlite")]
        "sqlite" => {
            let conn =
                llmrace::sqlite::open(&sqlite_path()).with_context(|| "opening sqlite history")?;
            Ok(llmrace::sqlite::load(&conn).with_context(|| "reading sqlite history")?)
        }
        other => anyhow::bail!("unknown storage backend '{other}' (expected jsonl or sqlite)"),
    }
}

pub(crate) fn report_sibling(out: &str, ext: &str) -> String {
    let path = std::path::Path::new(out);
    if path.extension().is_some() {
        // `--out race.md` → race.json; `--out probe-eu.json` stays probe-eu.json
        path.with_extension(ext).to_string_lossy().into_owned()
    } else {
        format!("{out}.{ext}")
    }
}

fn resolve_providers(cli: &Cli) -> Result<Vec<ProviderSpec>> {
    let mut out: Vec<ProviderSpec> = Vec::new();
    if let (Some(endpoint), Some(name)) = (&cli.endpoint, &cli.provider_name) {
        let model = resolve_model(&cli.model, "custom", "default");
        out.push(ProviderSpec::custom(
            name,
            endpoint,
            cli.api_key_env.as_deref(),
            &model,
        ));
    }
    if !cli.providers.is_empty() {
        for arg in &cli.providers {
            // `provider=model` entries race that provider with a specific
            // model; the same provider may appear multiple times with
            // different models.
            let (id, model_override) = match arg.split_once('=') {
                Some((id, m)) if !id.is_empty() && !m.is_empty() => (id, Some(m)),
                _ => (arg.as_str(), None),
            };
            let mut spec = ProviderSpec::find(id)
                .with_context(|| format!("unknown provider '{arg}' (try --list)"))?;
            if let Some(m) = model_override {
                spec.default_model = Arc::from(m);
            }
            out.push(spec);
        }
        return Ok(out);
    }
    if out.is_empty() {
        let available: Vec<ProviderSpec> = ProviderSpec::registry()
            .into_iter()
            .filter(|p| *p.id != *"mock" && !p.opt_in && p.is_available())
            .collect();
        if available.is_empty() {
            anyhow::bail!(
                "no providers configured; set an API key (e.g. OPENAI_API_KEY), start Ollama, or name providers explicitly"
            );
        }
        return Ok(available);
    }
    Ok(out)
}

fn apply_profile(
    mut cli: Cli,
    explicit: &dyn Fn(&str) -> bool,
) -> Result<(Cli, Option<llmrace::config::Config>)> {
    let Some(name) = cli.profile.clone() else {
        return Ok((cli, None));
    };
    let config = llmrace::config::Config::discover().with_context(|| "loading llmrace.toml")?;
    let profile = config
        .profile(&name)
        .with_context(|| format!("profile '{name}'"))?
        .clone();

    if cli.providers.is_empty() && !profile.providers.is_empty() {
        cli.providers = profile.providers.clone();
    }
    if cli.model.is_empty() {
        if let Some(model) = &profile.model {
            cli.model = vec![model.clone()];
        }
    }
    if !explicit("prompt") {
        if let Some(prompt) = &profile.prompt {
            cli.prompt = prompt.clone();
        }
    }
    if !explicit("runs") {
        if let Some(v) = profile.runs {
            cli.runs = v;
        }
    }
    if !explicit("warmup") {
        if let Some(v) = profile.warmup {
            cli.warmup = v;
        }
    }
    if !explicit("concurrency") {
        if let Some(v) = profile.concurrency {
            cli.concurrency = v;
        }
    }
    if !explicit("max_tokens") {
        if let Some(v) = profile.max_tokens {
            cli.max_tokens = v;
        }
    }
    if !explicit("timeout") {
        if let Some(v) = profile.timeout {
            cli.timeout = v;
        }
    }
    if !explicit("retries") {
        if let Some(v) = profile.retries {
            cli.retries = v;
        }
    }
    println!("\u{2699}  profile '{name}' loaded");
    Ok((cli, Some(config)))
}
