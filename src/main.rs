use anyhow::{Context, Result};
use bench::{BenchConfig, LaneUpdate, ProviderSummary};
use clap::Parser;
use llmrace::{bench, provider::Provider, report, tui};
use std::sync::{mpsc, Arc};

#[derive(Parser)]
#[command(
    name = "llmrace",
    version,
    about = "🏁 Race LLM providers head-to-head in your terminal",
    long_about = "llmrace benchmarks streaming latency, throughput and cost across LLM providers (OpenAI, Anthropic, Groq, OpenRouter, local Ollama) with a live TUI race and a shareable markdown report."
)]
struct Cli {
    providers: Vec<String>,

    #[arg(
        short,
        long,
        default_value = "Write one vivid paragraph about a lighthouse at dawn."
    )]
    prompt: String,

    #[arg(short = 'n', long, default_value_t = 3)]
    runs: usize,

    #[arg(long, default_value_t = 256)]
    max_tokens: u64,

    #[arg(long, default_value_t = 1)]
    warmup: usize,

    #[arg(short, long)]
    model: Option<String>,

    #[arg(long)]
    no_tui: bool,

    #[arg(long)]
    json: bool,

    #[arg(short, long, default_value = "llmrace-report.md")]
    out: String,

    #[arg(long)]
    list: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    if cli.list {
        println!("{:<12} {:<24} NEEDS", "PROVIDER", "DEFAULT MODEL");
        for p in Provider::registry() {
            println!(
                "{:<12} {:<24} {}",
                p.name,
                p.default_model,
                p.api_key_env.unwrap_or("nothing (local)")
            );
        }
        return Ok(());
    }

    let selected = resolve_providers(&cli)?;

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .context("failed to build http client")?;

    let (tx, rx) = mpsc::channel::<LaneUpdate>();
    let lanes: Vec<(String, String)> = selected
        .iter()
        .map(|p| {
            (
                p.name.to_string(),
                cli.model
                    .clone()
                    .unwrap_or_else(|| p.default_model.to_string()),
            )
        })
        .collect();

    println!(
        "🏁 llmrace v{} — {} providers × {} runs ({} warmup)",
        env!("CARGO_PKG_VERSION"),
        selected.len(),
        cli.runs,
        cli.warmup
    );
    println!("   prompt: \"{}\"\n", cli.prompt.replace('\n', " "));

    let mut handles = Vec::new();
    let client = Arc::new(client);
    for p in selected {
        let client = Arc::clone(&client);
        let cfg = BenchConfig {
            prompt: cli.prompt.clone(),
            max_tokens: cli.max_tokens,
            runs: cli.runs,
            warmup: cli.warmup,
        };
        let model = cli
            .model
            .clone()
            .unwrap_or_else(|| p.default_model.to_string());
        let tx = tx.clone();
        let prompt = cli.prompt.clone();
        let lane_client = client.as_ref().clone();
        handles.push(tokio::spawn(async move {
            bench::run_lane(lane_client, p, model, cfg, prompt, move |u| {
                let _ = tx.send(u);
            })
            .await
        }));
    }
    drop(tx);

    let summaries: Vec<ProviderSummary> = if cli.no_tui {
        let mut all = Vec::new();
        for h in handles {
            match h.await {
                Ok(s) => {
                    print_lane_progress(&s);
                    all.push(s);
                }
                Err(e) => eprintln!("lane panicked: {e}"),
            }
        }
        all
    } else {
        let prompt_for_tui = cli.prompt.clone();
        let tui_handle = tokio::task::spawn_blocking(move || {
            tui::Tui::new(&lanes, prompt_for_tui, cli.runs).run(rx)
        });
        let mut all = Vec::new();
        for h in handles {
            match h.await {
                Ok(s) => all.push(s),
                Err(e) => eprintln!("lane panicked: {e}"),
            }
        }
        match tui_handle.await {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => return Err(e),
            Err(e) => return Err(e.into()),
        }
        all
    };

    let table = report::terminal_table(&summaries);
    println!("\n{table}");

    let md = report::markdown(&summaries, &cli.prompt, cli.runs, cli.max_tokens);
    std::fs::write(&cli.out, &md).with_context(|| format!("writing {}", cli.out))?;
    println!("📄 markdown report saved to {}", cli.out);

    if cli.json {
        let j = report::json_report(&summaries, &cli.prompt, cli.runs, cli.max_tokens);
        let path = cli.out.replace(".md", ".json");
        std::fs::write(&path, j).with_context(|| format!("writing {}", path))?;
        println!("📊 json report saved to {path}");
    }

    Ok(())
}

fn resolve_providers(cli: &Cli) -> Result<Vec<Provider>> {
    if !cli.providers.is_empty() {
        let mut out = Vec::new();
        for name in &cli.providers {
            out.push(
                Provider::find(name)
                    .with_context(|| format!("unknown provider '{name}' (try --list)"))?,
            );
        }
        return Ok(out);
    }
    let available: Vec<Provider> = Provider::registry()
        .into_iter()
        .filter(|p| p.name == "mock" || p.is_available())
        .collect();
    if available.is_empty() {
        anyhow::bail!(
            "no providers configured; set an API key (e.g. OPENAI_API_KEY) or run Ollama"
        );
    }
    Ok(available)
}

fn print_lane_progress(s: &ProviderSummary) {
    if let Some(e) = &s.error {
        println!("  ✗ {:<12} {}", s.name, e);
    } else {
        println!(
            "  ✓ {:<12} {:>6.1} tok/s   {:>6.0} ms mean   TTFB {:>5.0} ms",
            s.name,
            s.mean_tokens_per_sec(),
            s.mean_total_ms(),
            s.mean_ttfb_ms()
        );
    }
}
