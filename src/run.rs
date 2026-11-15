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
        println!("\u{1f6e0} tool-calling mode: weather task, argument validity tracked");
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

    let variants: Vec<Option<llmrace::variants::Variant>> = match &cfg.sweep {
        Some(spec) => llmrace::variants::parse(spec)
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
                variant.as_ref().map(|v| Arc::from(v.name)),
                resolve_model(&cfg.model_overrides, &p.id, &p.default_model),
            ));
        }
        let prompt = match variant {
            Some(v) => v.prompt.to_string(),
            None => cfg.bench.prompt.clone(),
        };
        for p in &cfg.providers {
            let model = resolve_model(&cfg.model_overrides, &p.id, &p.default_model);
            let (handle, rx) = spawn_lane(
                cfg.client.clone(),
                p.clone(),
                model,
                BenchConfig {
                    prompt: prompt.clone(),
                    variant: variant.as_ref().map(|v| Arc::from(v.name)),
                    ..cfg.bench.clone()
                },
            );
            handles.push(async move { handle.await.map_err(anyhow::Error::from) });
            receivers.push(rx);
        }
    }

    let summaries: Vec<LaneSummary> = if cfg.no_tui {
        drop(receivers);
        let mut all = Vec::new();
        for h in handles {
            match h.await {
                Ok(s) => {
                    if !cfg.quiet {
                        print_lane_progress(&s);
                    }
                    all.push(s);
                }
                Err(e) => eprintln!("lane failed: {e}"),
            }
        }
        all
    } else {
        let rounds_label = match cfg.bench.duration_secs {
            Some(secs) => format!("\u{2264}{secs}s"),
            None => format!("{} rounds", cfg.bench.runs),
        };
        collect_with_tui(lanes, handles, receivers, &cfg.prompt, &rounds_label).await?
    };

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
    std::fs::write(&cfg.out, &md).with_context(|| format!("writing {}", cfg.out))?;
    println!("\u{1f4c4} markdown report saved to {}", cfg.out);

    let sibling = |ext: &str| -> String {
        let path = std::path::Path::new(&cfg.out);
        if path.extension().is_some() {
            path.with_extension(ext).to_string_lossy().into_owned()
        } else {
            format!("{}.{ext}", cfg.out)
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
    Ok(!regression.any_regressed())
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

    let report = llmrace::watch::watch(
        cfg.providers.clone(),
        resolve_model(
            &cfg.model_overrides,
            &cfg.providers[0].id,
            &cfg.providers[0].default_model,
        ),
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
