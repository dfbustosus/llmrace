//! Live OpenRouter smoke tests. Ignored by default; opt in with:
//! `cargo test --test live_openrouter -- --ignored`
//!
//! Requires `OPENROUTER_API_KEY`. Costs a few cents per run and validates
//! the full HTTP+SSE path against the real gateway — including the models
//! the README demo races — not the local test server.

use futures_util::StreamExt;
use llmrace::bench::{spawn_lane, BenchConfig};
use llmrace::provider::{stream_completion, CompletionRequest, ProviderSpec};

fn key() -> Option<String> {
    std::env::var("OPENROUTER_API_KEY")
        .ok()
        .filter(|k| k.len() > 20)
}

#[tokio::test]
#[ignore = "live API: requires OPENROUTER_API_KEY and spends money"]
async fn openrouter_streams_real_tokens() {
    let Some(_key) = key() else {
        eprintln!("skipping: no OPENROUTER_API_KEY");
        return;
    };
    let mut stream = stream_completion(
        &reqwest::Client::new(),
        &ProviderSpec::openrouter(),
        &CompletionRequest {
            model: "anthropic/claude-haiku-4.5",
            prompt: "Say exactly: hello from the live test",
            max_tokens: 16,
            tools: false,
            tool_task: None,
            sampling: llmrace::provider::Sampling::default(),
            messages: &[],
            tool: None,
        },
    )
    .await
    .expect("live stream starts");
    let mut text = String::new();
    let mut usage = None;
    while let Some(ev) = stream.next().await {
        let ev = ev.expect("live event");
        if let Some(t) = ev.token {
            text.push_str(&t);
        }
        usage = ev.completion_tokens.or(usage);
    }
    assert!(text.to_lowercase().contains("hello"), "got: {text}");
    assert!(usage.unwrap_or(0) > 0, "provider usage expected");
}

#[tokio::test]
#[ignore = "live API: requires OPENROUTER_API_KEY and spends money"]
async fn openrouter_lane_reports_honest_ttfb_and_cost() {
    let Some(_key) = key() else {
        eprintln!("skipping: no OPENROUTER_API_KEY");
        return;
    };
    let (handle, _progress) = spawn_lane(
        reqwest::Client::new(),
        ProviderSpec::openrouter(),
        "deepseek/deepseek-chat-v3.1".into(),
        BenchConfig {
            // a paragraph gives the throughput sample enough tokens to be
            // meaningful; a single-word answer can honestly measure < 10
            // tok/s when the provider thinks longer than it streams
            prompt: "Write one vivid paragraph about a lighthouse at dawn.".into(),
            runs: 2,
            warmup: 1,
            ..BenchConfig::default()
        },
    );
    let summary = handle.await.expect("lane task");
    assert!(summary.successful(), "{:?}", summary.error);
    let ttfb = summary.mean_ttfb_ms();
    assert!(
        ttfb > 50.0,
        "gateway TTFB must include the network round trip, got {ttfb} ms"
    );
    assert!(
        summary.mean_tokens_per_sec() > 10.0,
        "a paragraph streams faster than 10 tok/s, got {}",
        summary.mean_tokens_per_sec()
    );
    assert!(
        summary.cost_per_run_usd().is_some(),
        "deepseek-chat-v3.1 is a priced row since 1.33.0"
    );
}
