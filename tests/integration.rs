use llmrace::provider::{parse_sse_chunk, Wire};

fn oai_chunk(data: &str) -> String {
    format!("data: {data}\n\n")
}

#[test]
fn parses_openai_streaming_deltas() {
    let chunk = format!(
        "{}{}{}",
        oai_chunk(r#"{"choices":[{"delta":{"content":"Hel"}}]}"#),
        oai_chunk(r#"{"choices":[{"delta":{"content":"lo"}}]}"#),
        oai_chunk(r#"{"choices":[{"delta":{}}],"usage":{"completion_tokens":2}}"#)
    );
    let events = parse_sse_chunk(&chunk, Wire::OpenAiCompat);
    assert_eq!(events.len(), 3);
}

#[test]
fn ignores_done_and_empty_deltas() {
    let chunk = format!(
        "{}{}",
        oai_chunk("[DONE]"),
        oai_chunk(r#"{"choices":[{"delta":{"content":""}}]}"#)
    );
    let events = parse_sse_chunk(&chunk, Wire::OpenAiCompat);
    assert!(events.is_empty());
}

#[test]
fn parses_anthropic_events() {
    let chunk = format!(
        "event: content_block_delta\ndata: {{\"type\":\"content_block_delta\",\"delta\":{{\"type\":\"text_delta\",\"text\":\"Hi\"}}}}\n\n{}\n\n",
        r#"event: message_delta
data: {"type":"message_delta","usage":{"output_tokens":17}}"#
    );
    let events = parse_sse_chunk(&chunk, Wire::Anthropic);
    assert_eq!(events.len(), 2);
}

#[tokio::test]
async fn mock_race_completes_with_usage() {
    use llmrace::bench::{BenchConfig, RunMetrics};
    use llmrace::provider::{stream_completion, CompletionRequest, Provider};

    let client = reqwest::Client::new();
    let provider = Provider::mock();
    let req = CompletionRequest {
        model: "mock-1",
        prompt: "hello",
        max_tokens: 32,
    };
    let mut stream = stream_completion(&client, &provider, &req).await.unwrap();
    let mut tokens = 0u64;
    let mut usage = None;
    use futures_util::StreamExt;
    while let Some(ev) = stream.next().await {
        let ev = ev.unwrap();
        if ev.token.is_some() {
            tokens += 1;
        }
        if let Some(n) = ev.completion_tokens {
            usage = Some(n);
        }
    }
    assert_eq!(tokens, 24);
    assert_eq!(usage, Some(24));

    let metrics = RunMetrics {
        total_ms: 1000.0,
        ttfb_ms: 200.0,
        tokens: 160,
        chars: 640,
        prompt_tokens: 0,
    };
    let _ = BenchConfig::default();
    assert!((metrics.tokens_per_sec() - 200.0).abs() < 1e-9);
}

#[tokio::test]
async fn full_mock_race_produces_report() {
    use llmrace::bench::{run_lane, BenchConfig, LaneUpdate};

    let provider = llmrace::provider::Provider::mock();
    let cfg = BenchConfig {
        prompt: "demo".into(),
        max_tokens: 32,
        runs: 2,
        warmup: 0,
    };
    let client = reqwest::Client::new();
    let summary = run_lane(
        client,
        provider,
        "mock-1".into(),
        cfg,
        "demo".into(),
        |_: LaneUpdate| {},
    )
    .await;
    assert!(
        summary.successful(),
        "mock lane should succeed: {:?}",
        summary.error
    );
    assert_eq!(summary.runs.len(), 2);
    assert!(summary.mean_tokens_per_sec() > 0.0);

    let report = llmrace::report::markdown(&[summary], "demo", 2, 32);
    assert!(report.contains("# 🏁 llmrace report"));
    assert!(report.contains("mock"));

    let json = llmrace::report::json_report(&[], "demo", 2, 32);
    assert!(serde_json::from_str::<serde_json::Value>(&json).is_ok());
}

#[test]
fn metrics_aggregation() {
    use llmrace::bench::{percentile, ProviderSummary, RunMetrics};

    let xs = vec![10.0, 20.0, 30.0, 40.0, 100.0];
    assert_eq!(percentile(&xs, 50.0), 30.0);
    assert_eq!(percentile(&xs, 95.0), 100.0);
    assert_eq!(percentile(&xs, 0.0), 10.0);

    let mut s = ProviderSummary {
        name: "openai".into(),
        model: "gpt-4o-mini".into(),
        runs: vec![
            RunMetrics {
                total_ms: 1000.0,
                ttfb_ms: 200.0,
                tokens: 160,
                chars: 640,
                prompt_tokens: 0,
            },
            RunMetrics {
                total_ms: 2000.0,
                ttfb_ms: 300.0,
                tokens: 340,
                chars: 1360,
                prompt_tokens: 0,
            },
        ],
        error: None,
        prompt_cache: "Explain zero-knowledge proofs in two paragraphs.".into(),
    };
    assert!((s.mean_tokens_per_sec() - 200.0).abs() < 1e-6);
    let cost = s
        .cost_per_run_usd()
        .expect("gpt-4o-mini should have pricing");
    assert!(cost > 0.0 && cost < 0.01, "cost sanity: {cost}");
    s.error = Some("boom".into());
    assert!(!s.successful());
}

#[test]
fn cost_estimates_sane() {
    use llmrace::cost::{estimate_prompt_tokens, format_usd, pricing};
    let t = estimate_prompt_tokens("Explain zero-knowledge proofs in two paragraphs.");
    assert!(t > 5.0 && t < 20.0, "estimate: {t}");
    assert_eq!(pricing("openai", "gpt-4o-mini"), Some((0.15, 0.60)));
    assert_eq!(pricing("ollama", "whatever"), None);
    assert_eq!(format_usd(0.000234), "$0.000234");
}
