//! Live-API smoke tests. Ignored by default; opt in with:
//! `cargo test --test live_openai -- --ignored`
//!
//! Requires `OPENAI_API_KEY`. Costs a few cents per run and validates the
//! full HTTP+SSE path against the real provider — not the local test server.

use futures_util::StreamExt;
use llmrace::bench::{spawn_lane, BenchConfig};
use llmrace::provider::{stream_completion, CompletionRequest, ProviderSpec};

fn key() -> Option<String> {
    std::env::var("OPENAI_API_KEY")
        .ok()
        .filter(|k| k.len() > 20)
}

#[tokio::test]
#[ignore = "live API: requires OPENAI_API_KEY and spends money"]
async fn openai_streams_real_tokens() {
    let Some(_key) = key() else {
        eprintln!("skipping: no OPENAI_API_KEY");
        return;
    };
    let mut stream = stream_completion(
        &reqwest::Client::new(),
        &ProviderSpec::openai(),
        &CompletionRequest {
            model: "gpt-4o-mini",
            prompt: "Say exactly: hello from the live test",
            max_tokens: 16,
            tools: false,
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
#[ignore = "live API: requires OPENAI_API_KEY and spends money"]
async fn openai_lane_reports_honest_ttfb() {
    let Some(_key) = key() else {
        eprintln!("skipping: no OPENAI_API_KEY");
        return;
    };
    let (handle, _progress) = spawn_lane(
        reqwest::Client::new(),
        ProviderSpec::openai(),
        "gpt-4o-mini".into(),
        BenchConfig {
            prompt: "Reply with a single word.".into(),
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
        "cloud TTFB must include the network round trip, got {ttfb} ms"
    );
    assert!(summary.mean_tokens_per_sec() > 10.0);
}
