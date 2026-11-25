//! Standalone informational modes: `--init`, `--doctor`, `--list`,
//! `--dashboard`, `--trend`, `--compare`, `--merge`, `--completions`,
//! `--man`. Each handler declares only the inputs it needs, so mode
//! logic stays decoupled from the full CLI surface.

use anyhow::{Context, Result};

/// `llmrace --init`
pub(crate) fn init() -> Result<()> {
    let path = std::path::Path::new("llmrace.toml");
    if path.exists() {
        anyhow::bail!("llmrace.toml already exists — edit it or remove it first");
    }
    std::fs::write(path, include_str!("../examples/llmrace.toml"))
        .with_context(|| "writing llmrace.toml")?;
    println!("Created llmrace.toml with example profiles (fast, overnight, saturation, repro, ci)");
    println!("Edit it, then run: llmrace --profile fast");
    Ok(())
}

/// `llmrace --doctor [--json]`
pub(crate) async fn doctor(json: bool) -> Result<()> {
    let client = llmrace::provider::benchmark_client(std::time::Duration::from_secs(5), 1)?;
    let mut checks = vec![
        llmrace::doctor::check_config(),
        llmrace::doctor::check_history(),
    ];
    checks.push(llmrace::doctor::check_ollama(&client).await);
    for spec in llmrace::provider::ProviderSpec::registry()
        .iter()
        .filter(|p| p.opt_in)
    {
        checks.push(llmrace::doctor::check_local_daemon(&client, &spec.id, &spec.endpoint).await);
    }
    checks.extend(
        llmrace::doctor::check_providers(&client, &llmrace::provider::ProviderSpec::registry())
            .await,
    );
    checks.push(llmrace::doctor::check_version(&client).await);
    if json {
        let payload: Vec<serde_json::Value> = checks
            .iter()
            .map(|c| {
                serde_json::json!({
                    "name": c.name,
                    "status": match c.status {
                        llmrace::doctor::Status::Pass => "pass",
                        llmrace::doctor::Status::Warn => "warn",
                        llmrace::doctor::Status::Fail => "fail",
                    },
                    "detail": c.detail,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }
    print!("{}", llmrace::doctor::render(&checks));
    Ok(())
}

/// `llmrace --completions <shell>`
pub(crate) fn completions(shell: &str) -> Result<()> {
    let shell: clap_complete::Shell = shell
        .parse()
        .map_err(|e| anyhow::anyhow!("unknown shell '{shell}': {e}"))?;
    let mut cmd = <crate::Cli as clap::CommandFactory>::command();
    let name = cmd.get_name().to_string();
    clap_complete::generate(shell, &mut cmd, name, &mut std::io::stdout());
    Ok(())
}

/// `llmrace --man`
pub(crate) fn man() -> Result<()> {
    let cmd = <crate::Cli as clap::CommandFactory>::command();
    let man = clap_mangen::Man::new(cmd);
    let mut buffer: Vec<u8> = Default::default();
    man.render(&mut buffer)?;
    use std::io::Write;
    std::io::stdout().write_all(&buffer)?;
    Ok(())
}

/// `llmrace --list [--json]`
pub(crate) fn list(json: bool) -> Result<()> {
    use llmrace::provider::ProviderSpec;
    if json {
        let providers: Vec<serde_json::Value> = ProviderSpec::registry()
            .iter()
            .map(|p| {
                serde_json::json!({
                    "id": p.id.as_ref(),
                    "endpoint": p.endpoint.as_ref(),
                    "api_key_env": p.api_key_env.as_deref(),
                    "default_model": p.default_model.as_ref(),
                    "available": p.is_available(),
                    "opt_in": p.opt_in,
                    "experimental": p.experimental,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&providers)?);
        return Ok(());
    }
    println!("{:<14} {:<30} NEEDS", "PROVIDER", "DEFAULT MODEL");
    for p in ProviderSpec::registry() {
        let needs = match (p.api_key_env.as_deref(), p.opt_in) {
            (Some(env), _) => env.to_string(),
            (None, true) => "nothing (opt-in local)".to_string(),
            (None, false) => "nothing (local)".to_string(),
        };
        let marker = if p.experimental {
            " [experimental]"
        } else {
            ""
        };
        println!("{:<14} {:<30} {}{}", p.id, p.default_model, needs, marker);
    }
    Ok(())
}

/// `llmrace --dashboard [--out file.html]`
pub(crate) fn dashboard(days: u64, out: &str, storage: &str) -> Result<()> {
    let records = crate::load_history(storage)?;
    let now = unix_now();
    let html = llmrace::dashboard::render_html(&records, days, now);
    let path = if out == "llmrace-report.md" {
        std::path::PathBuf::from("llmrace-dashboard.html")
    } else {
        std::path::PathBuf::from(out)
    };
    std::fs::write(&path, html).with_context(|| format!("writing {}", path.display()))?;
    println!("\u{1f5a5} dashboard written to {}", path.display());
    Ok(())
}

/// `llmrace --trend [--days N] [providers…]`
pub(crate) fn trend(days: u64, providers: &[String], storage: &str) -> Result<()> {
    let records = crate::load_history(storage)?;
    let trends = llmrace::history::build_trends(&records, days, unix_now(), providers);
    print!("{}", llmrace::history::render_trend_table(&trends, days));
    Ok(())
}

/// `llmrace --compare a.json,b.json`
pub(crate) fn compare(paths: &[String], max_regression_pct: f64) -> Result<()> {
    let (a_raw, b_raw) = (
        std::fs::read_to_string(&paths[0]).with_context(|| format!("reading {}", paths[0]))?,
        std::fs::read_to_string(&paths[1]).with_context(|| format!("reading {}", paths[1]))?,
    );
    let a = llmrace::regression::BaselineReport::from_json_str(&a_raw)
        .with_context(|| format!("parsing {}", paths[0]))?;
    let b = llmrace::regression::BaselineReport::from_json_str(&b_raw)
        .with_context(|| format!("parsing {}", paths[1]))?;
    let report = llmrace::regression::compare_baselines(&b, &a, max_regression_pct);
    println!(
        "\u{2696}  comparing {} (baseline) vs {} (current)\n",
        paths[0], paths[1]
    );
    println!("{}", report.terminal_table(max_regression_pct));
    Ok(())
}

/// `llmrace --merge a.json,b.json,… [--json --out path]`
pub(crate) fn merge(paths: &[String], json: bool, out: &str) -> Result<()> {
    let paths: Vec<std::path::PathBuf> = paths.iter().map(Into::into).collect();
    let lanes = llmrace::merge::load_files(&paths)?;
    println!("{}", llmrace::merge::render_table(&lanes));
    if json {
        let merged = serde_json::json!({
            "tool": "llmrace",
            "version": env!("CARGO_PKG_VERSION"),
            "merged": lanes,
        });
        let path = crate::report_sibling(out, "json");
        std::fs::write(&path, serde_json::to_string_pretty(&merged)?)
            .with_context(|| format!("writing {path}"))?;
        println!("\u{1f30d} merged report saved to {path}");
    }
    Ok(())
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `llmrace --models [provider]`
pub(crate) async fn models(providers: Vec<llmrace::provider::ProviderSpec>) -> Result<()> {
    let client = llmrace::provider::benchmark_client(std::time::Duration::from_secs(10), 1)?;
    for spec in providers {
        match llmrace::provider::list_models(&client, &spec).await {
            Ok(ids) if ids.is_empty() => println!("{}: no models reported", spec.id),
            Ok(ids) => {
                println!("{} ({}):", spec.id, ids.len());
                for id in ids {
                    println!("  {id}");
                }
            }
            Err(e) => println!("{}: {e}", spec.id),
        }
    }
    Ok(())
}

/// `llmrace --suite <file>`: run each suite entry sequentially.
pub(crate) async fn run_suite(
    suite: llmrace::suite::SuiteFile,
    cli: &crate::Cli,
) -> anyhow::Result<()> {
    let mut results: Vec<llmrace::suite::SuiteResult> = Vec::new();

    // Shared race configuration, built once by the same builder the race
    // path uses. Entries clone it and override what they define.
    let mut base_cli = cli.clone();
    if base_cli.providers.is_empty() {
        base_cli.providers = suite
            .suites
            .first()
            .map(|entry| entry.providers.clone())
            .unwrap_or_default();
    }
    let base = crate::build_run_config(base_cli, Default::default())?;

    for (idx, entry) in suite.suites.iter().enumerate() {
        println!(
            "\n▸ suite {}/{} — {} ({})",
            idx + 1,
            suite.suites.len(),
            entry.name,
            entry.description
        );

        // resolve the prompt. A workload replaces the single string.
        let workload = if let Some(path) = &entry.workload {
            llmrace::workload::load(std::path::Path::new(path))
                .map_err(|e| anyhow::anyhow!("{e}"))?
        } else {
            Vec::new()
        };
        let prompt = if let Some(first) = workload.first() {
            first.prompt.clone()
        } else if let Some(p) = &entry.prompt {
            p.clone()
        } else if let Some(pf) = &entry.prompt_file {
            std::fs::read_to_string(pf)
                .map_err(|e| anyhow::anyhow!("reading prompt file {pf}: {e}"))?
        } else {
            cli.prompt.clone()
        };
        let prompt_label = if let Some(path) = &entry.workload {
            format!("workload {path} ({} prompts)", workload.len())
        } else {
            prompt.clone()
        };

        // resolve providers (positional args override the file)
        let providers: Vec<String> = if !cli.providers.is_empty() {
            cli.providers.clone()
        } else {
            entry.providers.clone()
        };

        // resolve provider specs
        let mut specs: Vec<llmrace::provider::ProviderSpec> = Vec::new();
        for name in &providers {
            let Some(spec) = llmrace::provider::ProviderSpec::parse(name) else {
                anyhow::bail!("unknown provider '{name}' (try --list)");
            };
            specs.push(spec);
        }
        if specs.is_empty() {
            anyhow::bail!("suite entry '{}' has no providers", entry.name);
        }

        // per-suite output file so each race's reports are preserved:
        // race.md becomes race-<suite-name>.md
        let suite_out = {
            let stem = cli
                .out
                .strip_suffix(".md")
                .unwrap_or(cli.out.trim_end_matches('.'));
            format!("{stem}-{}.md", entry.name.replace(' ', "-"))
        };

        // one builder for the whole RunConfig: the suite only overrides
        // what the entry itself defines, so a flag added to the race path
        // (weights, warmup mode, retries, gates, new report formats)
        // reaches suites automatically instead of drifting
        let mut cfg = base.clone();
        cfg.client = llmrace::provider::benchmark_client(
            std::time::Duration::from_secs(cli.timeout),
            entry.concurrency,
        )?;
        cfg.providers = specs;
        cfg.prompt = prompt_label;
        cfg.bench.prompt = prompt;
        cfg.bench.workload = workload;
        cfg.bench.runs = entry.runs.max(1);
        cfg.bench.warmup = entry.warmup;
        cfg.bench.concurrency = entry.concurrency.max(1);
        cfg.bench.max_tokens = entry.max_tokens;
        cfg.bench.tools = entry.tools;
        cfg.bench.tool_task = entry
            .tool_task
            .clone()
            .unwrap_or_else(|| "weather".to_string());
        cfg.bench.duration_secs = None;
        cfg.sweep = entry.sweep.clone();
        cfg.ramp = None;
        cfg.watch = false;
        cfg.no_tui = true;
        cfg.out = suite_out;

        // run headless, collect results
        let (exit, lane_summaries) = crate::run::run_race(cfg).await?;
        // run_race returns an exit code; for the suite we want the summaries.
        // The simplest correct approach: read the JSON report it wrote.
        let failed = exit != 0;
        results.push(llmrace::suite::SuiteResult {
            name: entry.name.clone(),
            description: entry.description.clone(),
            summaries: lane_summaries,
            failed,
        });
    }

    // combined report
    let report = llmrace::suite::combined_report(&results);
    let path = if cli.out.ends_with(".md") {
        cli.out.replace(".md", "-suite.md")
    } else {
        format!("{}-suite.md", cli.out)
    };
    std::fs::write(&path, report).with_context(|| format!("writing {path}"))?;
    println!("\n📄 suite report saved to {path}");

    if results.iter().any(|r| r.failed) {
        std::process::exit(1);
    }
    Ok(())
}
