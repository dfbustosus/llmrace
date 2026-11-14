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
    println!("Created llmrace.toml with example profiles (fast, overnight, saturation, ci)");
    println!("Edit it, then run: llmrace --profile fast");
    Ok(())
}

/// `llmrace --doctor [--json]`
pub(crate) async fn doctor(json: bool) -> Result<()> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .context("failed to build http client")?;
    let mut checks = vec![
        llmrace::doctor::check_config(),
        llmrace::doctor::check_history(),
    ];
    checks.push(llmrace::doctor::check_ollama(&client).await);
    checks.push(llmrace::doctor::check_local_daemon(&client, "lmstudio", 1234).await);
    checks.push(llmrace::doctor::check_local_daemon(&client, "llamacpp", 8080).await);
    checks.extend(
        llmrace::doctor::check_providers(&client, &llmrace::provider::ProviderSpec::registry())
            .await,
    );
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
        println!("{:<14} {:<30} {}", p.id, p.default_model, needs);
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
