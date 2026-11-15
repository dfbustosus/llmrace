//! Race execution: orchestration, reporting, gates and watch mode.
//!
//! [`RunConfig`] carries everything the race path needs — the binary's CLI
//! details stay behind this boundary, so execution depends on plain data
//! (dependency inversion at the binary edge).

use anyhow::{Context, Result};
use llmrace::bench::{spawn_lane, BenchConfig, LaneSummary};
use llmrace::provider::ProviderSpec;
use llmrace::score::Weights;
use llmrace::{report, tui};
use std::sync::Arc;

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

/// Spawns every lane (variants × providers) and collects the summaries.
///
/// `bench_override` replaces the base `BenchConfig` (used by `--ramp` to
/// race the same lanes at several concurrency levels); `headless_progress`
/// prints per-lane progress lines instead of spawning a TUI (ramp phases).
async fn race_collect(
    cfg: &RunConfig,
    bench_override: Option<BenchConfig>,
    headless_progress: bool,
) -> Result<Vec<LaneSummary>> {
    let bench = bench_override.unwrap_or_else(|| cfg.bench.clone());
    let variants: Vec<Option<llmrace::variants::VariantDef>> = match &cfg.sweep {
        Some(spec) => llmrace::variants::parse_with_custom(spec, &cfg.custom_variants)
            .map(|vs| vs.into_iter().map(Some).collect())
            .map_err(|e| anyhow::anyhow!("{e}"))?,
        None => vec![None],
    };

    let mut lanes: Vec<(Arc<str>, Option<Arc<str>>, String)> = Vec::new();
    let mut handles = Vec::new();
    let mut receivers = Vec::new();
    for variant in &variants {
        for p in &cfg.providers {
            lanes.push((
                Arc::clone(&p.id),
                variant.as_ref().map(|v| Arc::from(v.name.as_str())),
                resolve_model(&cfg.model_overrides, &p.id, &p.default_model),
            ));
        }
        let prompt = match variant {
            Some(v) => v.prompt.to_string(),
            None => bench.prompt.clone(),
        };
        for p in &cfg.providers {
            let model = resolve_model(&cfg.model_overrides, &p.id, &p.default_model);
            let (handle, rx) = spawn_lane(
                cfg.client.clone(),
                p.clone(),
                model,
                BenchConfig {
                    prompt: prompt.clone(),
                    variant: variant.as_ref().map(|v| Arc::from(v.name.as_str())),
                    ..bench.clone()
                },
            );
            handles.push(async move { handle.await.map_err(anyhow::Error::from) });
            receivers.push(rx);
        }
    }

    if headless_progress || cfg.no_tui {
        drop(receivers);
        let mut all = Vec::new();
        for h in handles {
            match h.await {
                Ok(s) => {
                    if headless_progress && !cfg.quiet {
                        print_lane_progress(&s);
                    }
                    all.push(s);
                }
                Err(e) => eprintln!("lane failed: {e}"),
            }
        }
        Ok(all)
    } else {
        let rounds_label = match bench.duration_secs {
            Some(secs) => format!("\u{2264}{secs}s"),
            None => format!("{} rounds", bench.runs),
        };
        collect_with_tui(lanes, handles, receivers, &cfg.prompt, &rounds_label).await
    }
}

/// Runs one full race (or sweep) and returns the process exit code.
pub(crate) async fn run_race(cfg: RunConfig) -> Result<i32> {
    if !cfg.quiet {
        println!(
            "\u{1f3c1} llmrace v{} — {} providers × {} rounds ({} warmup, c={})",
            env!("CARGO_PKG_VERSION"),
            cfg.providers.len(),
            cfg.bench.runs,
            cfg.bench.warmup,
            cfg.bench.concurrency
        );
        println!("   prompt: \"{}\"\n", cfg.prompt.replace('\n', " "));
    }
    if cfg.bench.tools {
        println!(
            "\u{1f6e0} tool-calling mode: {} task, argument validity tracked",
            cfg.bench.tool_task
        );
    }

    // Reasoning models spend the token budget thinking first; a small cap
    // yields almost no visible output (live-verified: gpt-5-nano returned 1
    // visible token with --max-tokens 256).
    let reasoning_capped = cfg
        .providers
        .iter()
        .any(|p| llmrace::provider::is_reasoning_model(&p.default_model))
        && cfg.bench.max_tokens < 512;
    if reasoning_capped && !cfg.quiet {
        println!(
            "\u{1f9e0} note: reasoning models think before the first visible token — consider --max-tokens 512+ so the answer isn't starved"
        );
    }

    let summaries = race_collect(&cfg, None, false).await?;

    if !cfg.quiet {
        let table = report::terminal_table(&summaries);
        println!("\n{table}");
        if cfg.winner {
            let groups = llmrace::score::score_groups(&summaries, &cfg.weights);
            print!("{}", llmrace::score::podiums(&groups, &cfg.weights));
        }
    }

    write_reports(&summaries, &cfg)?;
    export_metrics(&summaries, &cfg).await?;
    if cfg.record {
        record_history(&summaries, &cfg.storage)?;
    }

    // A run where every lane failed must exit non-zero: the TTFB and
    // baseline gates only judge successful lanes and would otherwise
    // pass over a total outage.
    if llmrace::gates::all_lanes_failed(&summaries) {
        eprintln!("\n✗ every provider failed — no metrics were measured");
        return Ok(1);
    }

    if !ttfb_gate(&summaries, &cfg) {
        return Ok(1);
    }
    if !regression_gate(&summaries, &cfg)? {
        return Ok(1);
    }
    Ok(0)
}

/// Collects lane results while the live TUI renders.
async fn collect_with_tui(
    lanes: Vec<(Arc<str>, Option<Arc<str>>, String)>,
    handles: Vec<
        impl std::future::Future<Output = std::result::Result<LaneSummary, anyhow::Error>>,
    >,
    receivers: Vec<std::sync::mpsc::Receiver<llmrace::bench::LaneUpdate>>,
    prompt: &str,
    rounds_label: &str,
) -> Result<Vec<LaneSummary>> {
    let merged = std::sync::mpsc::channel::<llmrace::bench::LaneUpdate>();
    for rx in receivers {
        let tx = merged.0.clone();
        tokio::spawn(async move {
            while let Ok(u) = rx.recv() {
                if tx.send(u).is_err() {
                    break;
                }
            }
        });
    }
    drop(merged.0);
    let tui = tui::Tui::with_rounds_label(lanes, prompt.to_string(), rounds_label.to_string());
    let rx_final = merged.1;
    let tui_handle = tokio::task::spawn_blocking(move || tui.run(rx_final).map(|_| ()));

    let mut all = Vec::new();
    for h in handles {
        match h.await {
            Ok(s) => all.push(s),
            Err(e) => eprintln!("lane failed: {e}"),
        }
    }
    match tui_handle.await {
        Ok(Ok(())) => Ok(all),
        Ok(Err(e)) => Err(e),
        Err(e) => Err(e.into()),
    }
}

/// Writes the markdown/JSON/CSV/HTML reports requested by the config.
fn write_reports(summaries: &[LaneSummary], cfg: &RunConfig) -> Result<()> {
    let mut md = report::markdown(summaries, &cfg.prompt, cfg.bench.runs, cfg.bench.max_tokens);
    if cfg.winner {
        let scored = llmrace::score::score_lanes(summaries, &cfg.weights);
        let block = llmrace::score::podium(&scored, &cfg.weights);
        if !block.is_empty() {
            md.push_str("\n## Podium\n");
            md.push_str(&block);
        }
    }
    if cfg.histogram {
        md.push_str(&report::histogram_section(summaries));
    }
    if cfg.out == "-" {
        // markdown to stdout (piping into CI summaries); sibling formats
        // fall back to the conventional filenames
        println!("{md}");
    } else {
        std::fs::write(&cfg.out, &md).with_context(|| format!("writing {}", cfg.out))?;
        println!("\u{1f4c4} markdown report saved to {}", cfg.out);
    }

    let sibling = |ext: &str| -> String {
        let base = if cfg.out == "-" {
            std::path::Path::new("llmrace-report")
        } else {
            std::path::Path::new(&cfg.out)
        };
        if base.extension().is_some() {
            base.with_extension(ext).to_string_lossy().into_owned()
        } else {
            format!("{}.{ext}", base.display())
        }
    };
    if cfg.json {
        let j = report::json_report_with_region(
            summaries,
            &cfg.prompt,
            cfg.bench.runs,
            cfg.bench.max_tokens,
            cfg.region.as_deref(),
        );
        let path = sibling("json");
        std::fs::write(&path, j).with_context(|| format!("writing {path}"))?;
        println!("\u{1f4ca} json report saved to {path}");
    }
    if cfg.csv {
        let c = report::csv(summaries);
        let path = sibling("csv");
        std::fs::write(&path, c).with_context(|| format!("writing {path}"))?;
        println!("\u{1f4c8} csv report saved to {path}");
    }
    if cfg.html {
        let scored = if cfg.winner {
            llmrace::score::score_lanes(summaries, &cfg.weights)
        } else {
            Vec::new()
        };
        let h = report::html_report(
            summaries,
            &cfg.prompt,
            cfg.bench.runs,
            cfg.bench.max_tokens,
            &scored,
        );
        let path = sibling("html");
        std::fs::write(&path, h).with_context(|| format!("writing {path}"))?;
        println!("\u{1f310} html report saved to {path}");
    }
    Ok(())
}

/// Pushes metrics to the Prometheus pushgateway when configured.
async fn export_metrics(summaries: &[LaneSummary], cfg: &RunConfig) -> Result<()> {
    if let Some(gateway) = &cfg.push_gateway {
        llmrace::prom::push(&cfg.client, gateway, summaries)
            .await
            .with_context(|| format!("pushing metrics to {gateway}"))?;
        println!("\u{1f4e1} metrics pushed to {gateway}");
    }
    Ok(())
}

/// Appends lanes to the persistent history (jsonl or sqlite backend).
fn record_history(summaries: &[LaneSummary], storage: &str) -> Result<()> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let records: Vec<llmrace::history::HistoryRecord> = summaries
        .iter()
        .filter(|s| s.successful())
        .map(|s| llmrace::history::record_from_summary(s, now))
        .collect();
    match storage {
        "jsonl" => {
            let path = llmrace::history::history_path();
            llmrace::history::append(&path, &records)
                .with_context(|| format!("recording history to {}", path.display()))?;
            println!(
                "\u{1f5c2} recorded {} lane(s) to {}",
                records.len(),
                path.display()
            );
        }
        #[cfg(feature = "sqlite")]
        "sqlite" => {
            let path = crate::sqlite_path();
            let conn = llmrace::sqlite::open(&path).with_context(|| "opening sqlite history")?;
            llmrace::sqlite::save(&conn, &records).with_context(|| "writing sqlite history")?;
            println!(
                "\u{1f5c2} recorded {} lane(s) to {}",
                records.len(),
                path.display()
            );
        }
        other => anyhow::bail!("unknown storage backend '{other}' (expected jsonl or sqlite)"),
    }
    Ok(())
}

/// `--max-ttfb` gate: returns `false` when any provider exceeded the
/// threshold (the process should exit non-zero).
fn ttfb_gate(summaries: &[LaneSummary], cfg: &RunConfig) -> bool {
    let Some(max_ttfb) = cfg.max_ttfb else {
        return true;
    };
    let offenders = llmrace::gates::ttfb_offenders(summaries, max_ttfb);
    let verdict = llmrace::gates::render_ttfb_verdict(&offenders, max_ttfb, cfg.quiet);
    if verdict.failed() {
        eprintln!("\n{}", verdict.message());
        return false;
    }
    if !verdict.message().is_empty() {
        println!("{}", verdict.message());
    }
    true
}

/// `--baseline` regression gate: returns `false` on regression.
fn regression_gate(summaries: &[LaneSummary], cfg: &RunConfig) -> Result<bool> {
    let Some(baseline_path) = &cfg.baseline else {
        return Ok(true);
    };
    let raw = std::fs::read_to_string(baseline_path)
        .with_context(|| format!("reading baseline {baseline_path}"))?;
    let baseline = llmrace::regression::BaselineReport::from_json_str(&raw)
        .with_context(|| format!("parsing baseline {baseline_path}"))?;
    let regression = llmrace::regression::evaluate(summaries, &baseline, cfg.max_regression);
    println!("\n{}", regression.terminal_table(cfg.max_regression));
    if cfg.require_significance {
        if regression.any_regressed() && !regression.any_regressed_significant() {
            println!("\u{2705} threshold breached but no regression is statistically significant — treating as noise (--require-significance)");
        }
        Ok(!regression.any_regressed_significant())
    } else {
        Ok(!regression.any_regressed())
    }
}

fn print_lane_progress(s: &LaneSummary) {
    if let Some(e) = &s.error {
        println!("  \u{2717} {:<14} {}", s.name, e);
    } else {
        let ci = match s.ci95_total_ms() {
            Some((lo, hi)) => format!("\u{b1}{:.0}", (hi - lo) / 2.0),
            None => "—".to_string(),
        };
        println!(
            "  \u{2713} {:<14} {:>6.1} tok/s   {:>6.0} ms {}   TTFB {:>4} ms",
            s.name,
            s.mean_tokens_per_sec(),
            s.mean_total_ms(),
            ci,
            llmrace::stats::fmt_ms(s.mean_ttfb_ms())
        );
    }
}

/// `--watch`: re-runs the race on an interval, alerting on degradation,
/// and returns the process exit code.
pub(crate) async fn watch_race(cfg: RunConfig) -> Result<i32> {
    let watch_config = llmrace::watch::WatchConfig {
        every_secs: cfg.every_secs,
        max_regression_pct: cfg.max_regression,
        rounds: None,
        bench: cfg.bench.clone(),
    };

    println!(
        "\u{1f440} llmrace watch — every {}s, alert threshold {}% (Ctrl+C to stop)\n",
        cfg.every_secs, cfg.max_regression
    );

    let ctrl_c_flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_flag = std::sync::Arc::clone(&ctrl_c_flag);
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        stop_flag.store(true, std::sync::atomic::Ordering::Relaxed);
        println!("\n\u{23f9} interrupted — finishing current round, then writing reports…");
    });

    let watch_lanes: Vec<(llmrace::provider::ProviderSpec, String)> = cfg
        .providers
        .iter()
        .map(|p| {
            (
                p.clone(),
                resolve_model(&cfg.model_overrides, &p.id, &p.default_model),
            )
        })
        .collect();
    let report = llmrace::watch::watch_with_models(
        watch_lanes,
        watch_config,
        cfg.client.clone(),
        |round, summaries, alerts| {
            for s in summaries {
                if s.successful() {
                    println!(
                        "  r{round} \u{2713} {:<14} {:>6.1} tok/s   TTFB {:>5.0} ms",
                        s.name,
                        s.mean_tokens_per_sec(),
                        s.mean_ttfb_ms()
                    );
                } else {
                    println!(
                        "  r{round} \u{2717} {:<14} {}",
                        s.name,
                        s.error.clone().unwrap_or_default()
                    );
                }
            }
            for a in alerts {
                println!(
                    "  \u{26a0} ALERT r{} {} {} {:.0} \u{2192} {:.0} ms (+{:.0}%)",
                    a.round, a.provider, a.metric, a.baseline, a.current, a.delta_pct
                );
            }
        },
        move || ctrl_c_flag.load(std::sync::atomic::Ordering::Relaxed),
    )
    .await;

    println!(
        "\nsession: {} rounds, {} alert(s)",
        report.rounds.len(),
        report.alerts.len()
    );

    if cfg.record {
        let mut records = Vec::new();
        for summaries in &report.rounds {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            for s in summaries.iter().filter(|s| s.successful()) {
                records.push(llmrace::history::record_from_summary(s, now));
            }
        }
        let path = llmrace::history::history_path();
        llmrace::history::append(&path, &records)
            .with_context(|| format!("recording history to {}", path.display()))?;
        println!(
            "\u{1f5c2} recorded {} lane(s) to {}",
            records.len(),
            path.display()
        );
    }

    let last = report.last_round();
    if !last.is_empty() {
        let table = report::terminal_table(last);
        println!("\n{table}");
        let md = report::markdown(last, &cfg.prompt, cfg.bench.runs, cfg.bench.max_tokens);
        std::fs::write(&cfg.out, md).with_context(|| format!("writing {}", cfg.out))?;
        println!("\u{1f4c4} report saved to {}", cfg.out);
    }
    Ok(0)
}

/// One `--ramp` level and its lane summaries.
pub(crate) struct RampLevel {
    /// Concurrency raced at this level.
    pub concurrency: usize,
    /// Lane results at this level.
    pub summaries: Vec<LaneSummary>,
}

/// Parses a `--ramp` level list (`1,2,4,8`): values must be \u{2265} 1 and
/// at least two distinct levels must remain after sorting and deduplication.
pub(crate) fn parse_ramp_levels(spec: &str) -> anyhow::Result<Vec<usize>> {
    let mut levels: Vec<usize> = Vec::new();
    for part in spec.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let level: usize = part.parse().map_err(|_| {
            anyhow::anyhow!("invalid --ramp level '{part}' (expected integers like 1,2,4,8)")
        })?;
        if level == 0 {
            anyhow::bail!("--ramp levels must be >= 1");
        }
        levels.push(level);
    }
    levels.sort_unstable();
    levels.dedup();
    if levels.len() < 2 {
        anyhow::bail!("--ramp needs at least two distinct concurrency levels");
    }
    Ok(levels)
}

/// `--ramp`: races every provider at each concurrency level and renders the
/// saturation curve. Returns the process exit code.
pub(crate) async fn ramp_race(cfg: RunConfig, levels: &[usize]) -> Result<i32> {
    use std::fmt::Write as _;

    if !cfg.quiet {
        println!(
            "\u{1f3c1} llmrace v{} ramp — {} providers \u{d7} levels {:?} ({} rounds each)",
            env!("CARGO_PKG_VERSION"),
            cfg.providers.len(),
            levels,
            cfg.bench.runs
        );
        println!("   prompt: \"{}\"\n", cfg.prompt.replace('\n', " "));
    }

    let mut phases: Vec<RampLevel> = Vec::new();
    for (idx, level) in levels.iter().enumerate() {
        if !cfg.quiet {
            println!(
                "\u{25b8} phase {}/{} — concurrency {level}",
                idx + 1,
                levels.len()
            );
        }
        let mut bench = cfg.bench.clone();
        bench.concurrency = *level;
        let summaries = race_collect(&cfg, Some(bench), true).await?;
        phases.push(RampLevel {
            concurrency: *level,
            summaries,
        });
    }

    // curve table (terminal + markdown)
    let mut curve = String::from("| Provider |");
    let mut header = String::new();
    for level in levels {
        let _ = write!(header, " c={level} |");
    }
    curve.push_str(&header);
    curve.push_str(" peak |\n|---|");
    for _ in levels {
        curve.push_str("---:|");
    }
    curve.push_str("---:|\n");

    let mut curve_term = format!("{:<20}", "Provider");
    for level in levels {
        let _ = write!(curve_term, " {:>10}", format!("c={level}"));
    }
    curve_term.push_str("      peak\n");
    curve_term.push_str(&"-".repeat(curve_term.len()));
    curve_term.push('\n');

    // stable lane order from the first phase
    let order: Vec<(String, String)> = phases
        .first()
        .map(|p| {
            p.summaries
                .iter()
                .map(|s| (s.name.clone(), s.model.clone()))
                .collect()
        })
        .unwrap_or_default();

    let mut ramp_json: Vec<serde_json::Value> = Vec::new();
    for (name, model) in &order {
        let mut md_row = format!("| {name} |");
        let mut term_row = format!("{:<20}", name);
        let mut best: Option<(usize, f64)> = None;
        let mut per_level = serde_json::Map::new();
        for phase in &phases {
            let lane = phase
                .summaries
                .iter()
                .find(|s| s.name == *name && s.model == *model);
            let tps = lane
                .filter(|s| s.successful())
                .map(|s| s.mean_tokens_per_sec())
                .unwrap_or(0.0);
            if best.is_none_or(|(_, b)| tps > b) {
                best = Some((phase.concurrency, tps));
            }
            per_level.insert(
                phase.concurrency.to_string(),
                serde_json::json!((tps * 10.0).round() / 10.0),
            );
            let _ = write!(md_row, " {tps:.1} |");
            let _ = write!(term_row, " {tps:>10.1}");
        }
        let peak = best.unwrap_or((1, 0.0));
        // flag degradation: any later level >10% below peak
        let degraded = phases.iter().any(|ph| {
            ph.concurrency > peak.0
                && ph
                    .summaries
                    .iter()
                    .find(|s| s.name == *name && s.model == *model)
                    .filter(|s| s.successful())
                    .map(|s| s.mean_tokens_per_sec())
                    .is_some_and(|tps| tps < peak.1 * 0.9)
        });
        let peak_note = if degraded {
            format!("c={} ({:.1}) \u{2b07}", peak.0, peak.1)
        } else {
            format!("c={} ({:.1})", peak.0, peak.1)
        };
        let _ = writeln!(md_row, " {peak_note} |");
        let _ = writeln!(term_row, "  {peak_note}");
        curve.push_str(&md_row);
        curve_term.push_str(&term_row);
        ramp_json.push(serde_json::json!({
            "provider": name,
            "model": model,
            "tokens_per_sec_by_concurrency": per_level,
            "peak_concurrency": peak.0,
            "peak_tokens_per_sec": (peak.1 * 10.0).round() / 10.0,
            "degrades_past_peak": degraded,
        }));
    }

    if !cfg.quiet {
        println!("\n{curve_term}");
    }

    // reports: markdown curve section + structured json
    let mut md = String::from("# \u{1f3c1} llmrace ramp\n\n");
    md.push_str(&format!(
        "- **Prompt**: `\"{}\"`\n- **Levels**: {:?}\n- **Rounds per level**: {}\n",
        cfg.prompt.replace('\n', " "),
        levels,
        cfg.bench.runs
    ));
    md.push_str("\n## Saturation curve (tokens/s by concurrency)\n\n");
    md.push_str(&curve);
    md.push_str("\n> Peak marks each provider's highest measured throughput; \u{2b07} flags degradation (>10% drop) beyond the peak.\n");
    std::fs::write(&cfg.out, &md).with_context(|| format!("writing {}", cfg.out))?;
    println!("\u{1f4c4} ramp report saved to {}", cfg.out);

    if cfg.html {
        let h = ramp_html(&order, &phases);
        let path = crate::report_sibling(&cfg.out, "html");
        std::fs::write(&path, h).with_context(|| format!("writing {path}"))?;
        println!("\u{1f310} ramp html saved to {path}");
    }

    if cfg.json {
        let payload = serde_json::json!({
            "tool": "llmrace",
            "version": env!("CARGO_PKG_VERSION"),
            "mode": "ramp",
            "region": cfg.region,
            "prompt": cfg.prompt,
            "runs": cfg.bench.runs,
            "max_tokens": cfg.bench.max_tokens,
            "levels": levels,
            "ramp": ramp_json,
        });
        let path = crate::report_sibling(&cfg.out, "json");
        std::fs::write(&path, serde_json::to_string_pretty(&payload)?)
            .with_context(|| format!("writing {path}"))?;
        println!("\u{1f4ca} ramp json saved to {path}");
    }

    if llmrace::gates::all_lanes_failed(
        &phases
            .last()
            .map(|p| p.summaries.clone())
            .unwrap_or_default(),
    ) {
        eprintln!("\n\u{2717} every provider failed at the highest level");
        return Ok(1);
    }
    Ok(0)
}

/// Self-contained HTML page for a ramp: SVG throughput-by-concurrency
/// series per provider plus the curve table.
fn ramp_html(order: &[(String, String)], phases: &[RampLevel]) -> String {
    const W: f64 = 760.0;
    const H: f64 = 320.0;
    const PAD: f64 = 44.0;
    const COLORS: [&str; 6] = [
        "#3390ff", "#3fb950", "#d29922", "#f778ba", "#a371f7", "#39c5cf",
    ];

    let levels: Vec<usize> = phases.iter().map(|p| p.concurrency).collect();
    let tps = |name: &str, model: &str, level: usize| -> f64 {
        phases
            .iter()
            .find(|p| p.concurrency == level)
            .and_then(|p| {
                p.summaries
                    .iter()
                    .find(|s| s.name == name && s.model == model)
            })
            .filter(|s| s.successful())
            .map(|s| s.mean_tokens_per_sec())
            .unwrap_or(0.0)
    };
    let max_tps = order
        .iter()
        .flat_map(|(n, m)| levels.iter().map(move |l| tps(n, m, *l)))
        .fold(1.0, f64::max);
    let max_level = *levels.last().unwrap_or(&1) as f64;
    let x = |level: usize| PAD + (level as f64 / max_level) * (W - 2.0 * PAD);
    let y = |v: f64| H - PAD - (v / max_tps) * (H - 2.0 * PAD);

    let mut svg = format!(
        "<svg viewBox='0 0 {W} {H}' xmlns='http://www.w3.org/2000/svg' role='img' aria-label='saturation curve'>"
    );
    // gridlines at 25% steps
    for step in 1..=3 {
        let gy = y(max_tps * step as f64 / 4.0);
        svg.push_str(&format!(
            "<line x1='{PAD}' y1='{gy:.1}' x2='{:.1}' y2='{gy:.1}' stroke='#21262d'/>",
            W - PAD
        ));
        svg.push_str(&format!(
            "<text x='{}' y='{gy:.1}'>{:.0}</text>",
            PAD - 6.0,
            max_tps * step as f64 / 4.0
        ));
    }
    for level in &levels {
        svg.push_str(&format!(
            "<text x='{:.1}' y='{}' text-anchor='middle'>{level}</text>",
            x(*level),
            H - PAD + 14.0
        ));
    }
    svg.push_str(&format!(
        "<text x='{PAD}' y='{}'>c (concurrency)</text><text transform='rotate(-90)' x='{}' y='14'>tokens/s</text>",
        H - 8.0,
        -(H / 2.0),
    ));

    let mut legend = String::new();
    for (i, (name, model)) in order.iter().enumerate() {
        let color = COLORS[i % COLORS.len()];
        let points: Vec<String> = levels
            .iter()
            .map(|l| format!("{:.1},{:.1}", x(*l), y(tps(name, model, *l))))
            .collect();
        svg.push_str(&format!(
            "<polyline fill='none' stroke='{color}' stroke-width='2' points='{}'/>",
            points.join(" ")
        ));
        for l in &levels {
            svg.push_str(&format!(
                "<circle cx='{:.1}' cy='{:.1}' r='3' fill='{color}'/>",
                x(*l),
                y(tps(name, model, *l))
            ));
        }
        legend.push_str(&format!(
            "<li><span style='color:{color}'>\u{25cf}</span> {} ({})</li>",
            llmrace::util::html_escape(name),
            llmrace::util::html_escape(model)
        ));
    }
    svg.push_str("</svg>");

    // reuse the markdown curve table, converted loosely: rebuild as HTML
    let mut table = String::from("<table><tr><th>Provider</th>");
    for level in &levels {
        table.push_str(&format!("<th>c={level}</th>"));
    }
    table.push_str("<th>peak</th></tr>");
    for (name, model) in order {
        table.push_str(&format!(
            "<tr><td>{} ({})</td>",
            llmrace::util::html_escape(name),
            llmrace::util::html_escape(model)
        ));
        let mut peak = (1usize, 0.0);
        for level in &levels {
            let v = tps(name, model, *level);
            if v > peak.1 {
                peak = (*level, v);
            }
            table.push_str(&format!("<td>{v:.1}</td>"));
        }
        table.push_str(&format!(
            "<td><b>c={} ({:.1})</b></td></tr>",
            peak.0, peak.1
        ));
    }
    table.push_str("</table>");

    format!(
        "<!DOCTYPE html><html lang='en'><head><meta charset='utf-8'><meta name='viewport' content='width=device-width,initial-scale=1'><title>llmrace ramp</title><style>:root {{ color-scheme: dark; }}body {{ font-family: ui-monospace, 'SF Mono', Menlo, monospace; background: #0d1117; color: #e6edf3; margin: 0 auto; max-width: 820px; padding: 2rem 1rem; }}h1 {{ color: #3390ff; }} svg {{ width: 100%; height: auto; background: #161b22; border: 1px solid #21262d; border-radius: 6px; }}text {{ fill: #8b949e; font-size: 10px; }} table {{ border-collapse: collapse; width: 100%; margin-top: 1rem; }}th, td {{ border: 1px solid #21262d; padding: .3rem .6rem; text-align: left; }} th {{ color: #8b949e; }}ul {{ list-style: none; padding: 0; }} footer {{ color: #484f58; margin-top: 3rem; }}
</style></head><body><h1>\u{1f3c1} llmrace ramp</h1><p>throughput by concurrency — peak marks each provider's saturation point</p>{svg}<ul>{legend}</ul>{table}<footer>generated with <a style='color:#3390ff' href='https://github.com/dfbustosus/llmrace'>llmrace</a> v{} \u{1f980}</footer></body></html>",
        env!("CARGO_PKG_VERSION"),
    )
}
