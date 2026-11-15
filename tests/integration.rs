use futures_util::StreamExt;
use llmrace::bench::{spawn_lane, BenchConfig, RunMetrics};
use llmrace::provider::{decode_payload, stream_completion, CompletionRequest, ProviderSpec, Wire};
use llmrace::regression::{evaluate, BaselineReport};
use llmrace::sse::{parse_data_events, SseParser};
use llmrace::stats::Sample;
use llmrace::{cost, report};

#[test]
fn sse_parses_events_split_at_every_boundary() {
    let payload = "data: {\"a\":1}\n\ndata: {\"b\":2}\n\ndata: [DONE]\n\n";
    let bytes = payload.as_bytes();
    for split in 1..bytes.len() {
        let mut parser = SseParser::new();
        let mut blocks = parser.feed(&bytes[..split]).unwrap();
        blocks.extend(parser.feed(&bytes[split..]).unwrap());
        assert_eq!(blocks.len(), 3, "split at {split}");
    }
}

#[test]
fn sse_handles_crlf_and_mixed_separators() {
    let mut parser = SseParser::new();
    let blocks = parser
        .feed(b"data: one\r\n\r\ndata: two\n\ndata: three\r\n\r\n")
        .unwrap();
    assert_eq!(blocks.len(), 3);
    assert_eq!(parse_data_events(&blocks[0]), vec!["one"]);
    assert_eq!(parse_data_events(&blocks[1]), vec!["two"]);
    assert_eq!(parse_data_events(&blocks[2]), vec!["three"]);
}

#[test]
fn sse_finish_flushes_trailing_block() {
    let mut parser = SseParser::new();
    assert!(parser.feed(b"data: complete\n\n").unwrap().len() == 1);
    let flushed = parser.finish();
    assert!(flushed.is_empty());
    let mut parser = SseParser::new();
    parser.feed(b"data: complete\n\ndata: partial").unwrap();
    assert_eq!(parser.finish().len(), 1);
}

#[test]
fn sse_ignores_non_data_lines() {
    let mut parser = SseParser::new();
    let blocks = parser
        .feed(b": comment\nevent: x\nid: 7\ndata: payload\n\n")
        .unwrap();
    assert_eq!(blocks.len(), 1);
    assert_eq!(parse_data_events(&blocks[0]), vec!["payload"]);
}

#[test]
fn percentile_linear_interpolation_is_exact() {
    let s = Sample::from_values(vec![10.0, 20.0, 30.0, 40.0, 100.0]);
    assert!((s.percentile(50.0) - 30.0).abs() < 1e-12);
    assert!((s.percentile(0.0) - 10.0).abs() < 1e-12);
    assert!((s.percentile(100.0) - 100.0).abs() < 1e-12);
    assert!((s.percentile(95.0) - 88.0).abs() < 1e-12);
    assert!((s.percentile(25.0) - 20.0).abs() < 1e-12);
}

#[test]
fn t_ci95_width_matches_table() {
    let s = Sample::from_values(vec![1.0, 2.0, 3.0, 4.0, 5.0]);
    let (lo, hi) = s.t_ci95().unwrap();
    let mean = 3.0;
    let expected_half = 2.776 * s.stddev() / 5.0_f64.sqrt();
    assert!((mean - lo - expected_half).abs() < 1e-3);
    assert!((hi - mean - expected_half).abs() < 1e-3);
    assert!(Sample::new().t_ci95().is_none());
}

#[test]
fn outliers_flagged_beyond_three_sigma() {
    let mut values = vec![0.0; 20];
    values.push(100.0);
    let s = Sample::from_values(values);
    assert_eq!(s.outliers_3sigma(), vec![20]);
}

#[test]
fn openai_wire_decodes_deltas_and_usage() {
    let delta = decode_payload(
        r#"{"choices":[{"delta":{"content":"Hel"}}]}"#,
        Wire::OpenAiCompat,
    )
    .unwrap();
    assert_eq!(delta.token.as_deref(), Some("Hel"));

    let empty = decode_payload(
        r#"{"choices":[{"delta":{"content":""}}]}"#,
        Wire::OpenAiCompat,
    )
    .unwrap();
    assert!(empty.token.is_none());

    let usage = decode_payload(
        r#"{"choices":[{"delta":{}}],"usage":{"completion_tokens":2}}"#,
        Wire::OpenAiCompat,
    )
    .unwrap();
    assert_eq!(usage.completion_tokens, Some(2));

    assert!(decode_payload("[DONE]", Wire::OpenAiCompat).is_none());
}

#[test]
fn anthropic_wire_decodes_events() {
    let delta = decode_payload(
        r#"{"type":"content_block_delta","delta":{"type":"text_delta","text":"Hi"}}"#,
        Wire::Anthropic,
    )
    .unwrap();
    assert_eq!(delta.token.as_deref(), Some("Hi"));

    let usage = decode_payload(
        r#"{"type":"message_delta","usage":{"output_tokens":17}}"#,
        Wire::Anthropic,
    )
    .unwrap();
    assert_eq!(usage.completion_tokens, Some(17));

    assert!(decode_payload(r#"{"type":"ping"}"#, Wire::Anthropic).is_none());
}

#[test]
fn custom_provider_points_at_custom_endpoint() {
    let spec = ProviderSpec::custom(
        "vllm",
        "http://gpu-box:8000/v1/chat/completions",
        Some("VLLM_API_KEY"),
        "meta-llama/Llama-3.1-8B-Instruct",
    );
    assert_eq!(*spec.id, *"vllm");
    assert_eq!(
        spec.endpoint.as_ref(),
        "http://gpu-box:8000/v1/chat/completions"
    );
    assert_eq!(spec.wire, Wire::OpenAiCompat);
    assert_eq!(spec.api_key_env.as_deref(), Some("VLLM_API_KEY"));
}

#[tokio::test]
async fn mock_stream_emits_tokens_and_usage() {
    let client = reqwest::Client::new();
    let spec = ProviderSpec::mock();
    let req = CompletionRequest {
        model: "mock-1",
        prompt: "hello",
        max_tokens: 32,
        tools: false,
        tool_task: None,
    };
    let mut stream = stream_completion(&client, &spec, &req).await.unwrap();
    let mut tokens = 0u64;
    let mut usage = None;
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
}

#[tokio::test]
async fn full_mock_race_produces_reports() {
    let config = BenchConfig {
        prompt: "demo".into(),
        runs: 2,
        warmup: 0,
        ..BenchConfig::default()
    };
    let (handle, progress) = spawn_lane(
        reqwest::Client::new(),
        ProviderSpec::mock(),
        "mock-1".into(),
        config,
    );
    drop(progress);
    let summary = handle.await.unwrap();
    assert!(summary.successful(), "error: {:?}", summary.error);
    assert_eq!(summary.runs.len(), 2);
    assert!(summary.mean_tokens_per_sec() > 0.0);
    assert!(summary.ci95_total_ms().is_some());

    let md = report::markdown(std::slice::from_ref(&summary), "demo", 2, 32);
    assert!(md.contains("# 🏁 llmrace report"));
    assert!(md.contains("mock"));
    assert!(md.contains("±"));

    let csv = report::csv(std::slice::from_ref(&summary));
    assert!(csv.starts_with("provider,model,run,total_ms"));
    assert_eq!(csv.lines().count(), 3);

    let json = report::json_report(&[summary], "demo", 2, 32);
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed["providers"][0]["provider"], "mock");
}

#[tokio::test]
async fn concurrency_two_measures_saturation_throughput() {
    let config = BenchConfig {
        prompt: "demo".into(),
        runs: 1,
        warmup: 0,
        concurrency: 2,
        ..BenchConfig::default()
    };
    let (handle, progress) = spawn_lane(
        reqwest::Client::new(),
        ProviderSpec::mock(),
        "mock-1".into(),
        config,
    );
    drop(progress);
    let summary = handle.await.unwrap();
    assert!(summary.successful());
    assert_eq!(summary.runs.len(), 1);
    assert_eq!(summary.concurrency, 2);
    assert_eq!(summary.runs[0].tokens, 48);
    let single_stream = 24.0 / (0.276);
    assert!(
        summary.mean_tokens_per_sec() > single_stream * 1.5,
        "aggregate {} should approximate saturation throughput, not the per-stream {}",
        summary.mean_tokens_per_sec(),
        single_stream
    );
}

#[test]
fn run_metrics_generation_math() {
    let m = RunMetrics {
        total_ms: 1000.0,
        ttfb_ms: 200.0,
        tokens: 160,
        chars: 640,
        prompt_tokens: 0,
        ..RunMetrics::default()
    };
    assert!((m.gen_ms() - 800.0).abs() < 1e-9);
    assert!((m.tokens_per_sec() - 200.0).abs() < 1e-9);
}

#[test]
fn cost_estimates_and_pricing_sane() {
    let t = cost::estimate_prompt_tokens("Explain zero-knowledge proofs in two paragraphs.");
    assert!(t > 5.0 && t < 20.0, "estimate: {t}");
    assert_eq!(cost::pricing("openai", "gpt-4o-mini"), Some((0.15, 0.60)));
    assert_eq!(
        cost::pricing("anthropic", "claude-sonnet-4-5"),
        Some((3.0, 15.0))
    );
    assert_eq!(cost::pricing("custom-anything", "whatever"), None);
    assert_eq!(cost::format_usd(0.000234), "$0.000234");
}

fn regression_summary(
    name: &str,
    mean_ms: f64,
    p95_ms: f64,
    ttfb_ms: f64,
    tps: f64,
) -> llmrace::bench::LaneSummary {
    let gen_s = (mean_ms - ttfb_ms).max(0.0) / 1000.0;
    let run = RunMetrics {
        total_ms: mean_ms,
        ttfb_ms,
        tokens: (tps * gen_s).round() as u64,
        chars: 400,
        prompt_tokens: 0,
        ..RunMetrics::default()
    };
    let _ = p95_ms;
    llmrace::bench::LaneSummary {
        name: name.into(),
        model: "m".into(),
        runs: vec![run],
        error: None,
        prompt: "p".into(),
        concurrency: 1,
        variant: None,
        tool_task: None,
    }
}

#[test]
fn regression_passes_within_threshold() {
    let baseline_raw = r#"{"providers":[{"provider":"mock","model":"mock-1","mean_total_ms":100.0,"p95_total_ms":120.0,"mean_ttfb_ms":20.0,"mean_tokens_per_sec":80.0}]}"#;
    let baseline = BaselineReport::from_json_str(baseline_raw).unwrap();
    let current = vec![regression_summary("mock", 110.0, 130.0, 22.0, 76.0)];
    let report = evaluate(&current, &baseline, 15.0);
    assert!(!report.any_regressed());
    assert!(report.comparisons.len() == 1);
    assert!(report.new_providers.is_empty());
    assert!(report.skipped_providers.is_empty());
}

#[test]
fn regression_fails_beyond_threshold() {
    let baseline_raw = r#"{"providers":[{"provider":"mock","model":"mock-1","mean_total_ms":100.0,"p95_total_ms":120.0,"mean_ttfb_ms":20.0,"mean_tokens_per_sec":80.0}]}"#;
    let baseline = BaselineReport::from_json_str(baseline_raw).unwrap();

    let slow = vec![regression_summary("mock", 130.0, 150.0, 20.0, 78.0)];
    let report = evaluate(&slow, &baseline, 15.0);
    assert!(report.any_regressed());
    let mean = report.comparisons[0]
        .metrics
        .iter()
        .find(|m| m.label == "mean latency")
        .unwrap();
    assert!(mean.regressed);
    assert!((mean.delta_pct - 30.0).abs() < 1e-9);

    let dropped_tps = vec![regression_summary("mock", 100.0, 120.0, 20.0, 60.0)];
    let report = evaluate(&dropped_tps, &baseline, 15.0);
    assert!(report.any_regressed());
}

#[test]
fn regression_reports_new_and_skipped_providers() {
    let baseline_raw = r#"{"providers":[{"provider":"groq","model":"m","mean_total_ms":100.0,"mean_tokens_per_sec":80.0}]}"#;
    let baseline = BaselineReport::from_json_str(baseline_raw).unwrap();
    let current = vec![regression_summary("mock", 100.0, 100.0, 10.0, 80.0)];
    let report = evaluate(&current, &baseline, 15.0);
    assert_eq!(report.new_providers, vec!["mock".to_string()]);
    assert_eq!(report.skipped_providers, vec!["groq".to_string()]);
    assert!(!report.any_regressed());
    let table = report.terminal_table(15.0);
    assert!(table.contains("new provider"));
    assert!(table.contains("not benchmarked"));
}

#[test]
fn gemini_wire_decodes_deltas_and_usage() {
    let delta = decode_payload(
        r#"{"candidates":[{"content":{"parts":[{"text":"Hel"}]}}]}"#,
        Wire::Gemini,
    )
    .unwrap();
    assert_eq!(delta.token.as_deref(), Some("Hel"));

    let usage = decode_payload(
        r#"{"candidates":[],"usageMetadata":{"candidatesTokenCount":42}}"#,
        Wire::Gemini,
    )
    .unwrap();
    assert_eq!(usage.completion_tokens, Some(42));

    assert!(
        decode_payload(
            r#"{"candidates":[{"content":{"parts":[{"text":""}]}}]}"#,
            Wire::Gemini
        )
        .is_none()
            || decode_payload(
                r#"{"candidates":[{"content":{"parts":[{"text":""}]}}]}"#,
                Wire::Gemini
            )
            .map(|e| e.token.is_none())
            .unwrap_or(true)
    );
}

#[test]
fn gemini_registry_entry() {
    let spec = llmrace::provider::ProviderSpec::find("gemini").unwrap();
    assert_eq!(spec.wire, Wire::Gemini);
    assert_eq!(*spec.default_model, *"gemini-2.0-flash");
    assert_eq!(spec.api_key_env.as_deref(), Some("GEMINI_API_KEY"));
    assert!(spec.endpoint.contains("{model}"));
    assert_eq!(
        llmrace::cost::pricing("gemini", "gemini-2.0-flash"),
        Some((0.10, 0.40))
    );
}

#[test]
fn openai_compat_provider_batch_specs() {
    let cases = [
        ("deepseek", "DEEPSEEK_API_KEY", "deepseek-chat"),
        ("mistral", "MISTRAL_API_KEY", "mistral-small-latest"),
        (
            "together",
            "TOGETHER_API_KEY",
            "meta-llama/Llama-3.3-70B-Instruct-Turbo",
        ),
        (
            "fireworks",
            "FIREWORKS_API_KEY",
            "accounts/fireworks/models/llama-v3p3-70b-instruct",
        ),
        ("cerebras", "CEREBRAS_API_KEY", "llama-3.3-70b"),
    ];
    for (id, key_env, model) in cases {
        let spec = llmrace::provider::ProviderSpec::find(id)
            .unwrap_or_else(|| panic!("provider {id} missing from registry"));
        assert_eq!(spec.wire, Wire::OpenAiCompat, "{id}");
        assert_eq!(spec.api_key_env.as_deref(), Some(key_env), "{id}");
        assert_eq!(*spec.default_model, *model, "{id}");
        assert!(spec.endpoint.starts_with("https://"), "{id}");
    }
}

#[test]
fn openai_compat_provider_batch_pricing() {
    assert_eq!(
        llmrace::cost::pricing("deepseek", "deepseek-chat"),
        Some((0.27, 1.10))
    );
    assert_eq!(
        llmrace::cost::pricing("mistral", "mistral-small-latest"),
        Some((0.20, 0.60))
    );
    assert_eq!(
        llmrace::cost::pricing("cerebras", "llama-3.3-70b"),
        Some((0.85, 1.20))
    );
}

#[test]
fn sse_buffer_cap_rejects_separatorless_flood() {
    let mut parser = SseParser::new();
    let flood = vec![b'a'; SseParser::MAX_BUFFER_BYTES + 1];
    let err = parser.feed(&flood).expect_err("cap must trip");
    assert!(err.to_string().contains("buffer exceeded"));

    let mut parser = SseParser::new();
    let exact = vec![b'a'; SseParser::MAX_BUFFER_BYTES];
    assert!(parser.feed(&exact).is_ok());
}

#[test]
fn xai_and_perplexity_specs() {
    let xai = llmrace::provider::ProviderSpec::find("xai").unwrap();
    assert_eq!(xai.wire, Wire::OpenAiCompat);
    assert_eq!(*xai.default_model, *"grok-3-mini");
    assert_eq!(xai.api_key_env.as_deref(), Some("XAI_API_KEY"));

    let pplx = llmrace::provider::ProviderSpec::find("perplexity").unwrap();
    assert_eq!(pplx.wire, Wire::OpenAiCompat);
    assert_eq!(*pplx.default_model, *"sonar");
    assert_eq!(pplx.api_key_env.as_deref(), Some("PERPLEXITY_API_KEY"));

    assert_eq!(
        llmrace::cost::pricing("xai", "grok-3-mini"),
        Some((0.30, 0.50))
    );
    assert_eq!(
        llmrace::cost::pricing("perplexity", "sonar"),
        Some((1.00, 1.00))
    );
}

#[tokio::test]
async fn percentile_ladder_and_histogram_render() {
    let runs: Vec<RunMetrics> = (0..10)
        .map(|i| RunMetrics {
            total_ms: 100.0 + i as f64 * 50.0,
            ttfb_ms: 10.0,
            tokens: 10,
            chars: 40,
            prompt_tokens: 0,
            ..RunMetrics::default()
        })
        .collect();
    let summary = llmrace::bench::LaneSummary {
        name: "mock".into(),
        model: "m".into(),
        runs,
        error: None,
        prompt: "p".into(),
        concurrency: 1,
        variant: None,
        tool_task: None,
    };

    let ladder = summary.latency_percentiles();
    assert_eq!(ladder.len(), 7);
    assert_eq!(ladder[0].0, "p10");
    assert!((ladder[0].1 - 145.0).abs() < 1e-9);
    assert!((ladder[2].1 - 325.0).abs() < 1e-9);
    assert!((ladder[6].1 - 545.5).abs() < 1e-9);

    let section = report::histogram_section(std::slice::from_ref(&summary));
    assert!(section.contains("## Latency distribution"));
    assert!(section.contains("| mock | 145 ms |"));
    assert!(section.contains("▇"));
    assert!(section.contains(" ms │"));

    let sparse = llmrace::bench::LaneSummary {
        runs: Vec::new(),
        ..summary.clone()
    };
    assert!(report::histogram_section(std::slice::from_ref(&sparse)).is_empty());
}

#[test]
fn cohere_wire_decodes_v2_and_v1_shapes() {
    let v2 = decode_payload(
        r#"{"type":"content_delta","delta":{"message":{"content":{"type":"text","text":"Hi"}}}}"#,
        Wire::Cohere,
    )
    .unwrap();
    assert_eq!(v2.token.as_deref(), Some("Hi"));

    let v1 = decode_payload(
        r#"{"event_type":"text-generation","text":"legacy"}"#,
        Wire::Cohere,
    )
    .unwrap();
    assert_eq!(v1.token.as_deref(), Some("legacy"));

    let usage_v1 = decode_payload(
        r#"{"event_type":"stream-end","response":{"meta":{"billed_units":{"output_tokens":42}}}}"#,
        Wire::Cohere,
    )
    .unwrap();
    assert_eq!(usage_v1.completion_tokens, Some(42));

    let usage_v2 = decode_payload(
        r#"{"type":"usage","usage":{"tokens":{"output_tokens":7}}}"#,
        Wire::Cohere,
    )
    .unwrap();
    assert_eq!(usage_v2.completion_tokens, Some(7));

    assert!(decode_payload(r#"{"type":"frame-start"}"#, Wire::Cohere).is_none());

    let spec = llmrace::provider::ProviderSpec::find("cohere").unwrap();
    assert_eq!(spec.wire, Wire::Cohere);
    assert_eq!(*spec.default_model, *"command-a-03-2025");
    assert_eq!(
        llmrace::cost::pricing("cohere", "command-a-03-2025"),
        Some((2.50, 10.00))
    );
}

#[tokio::test]
async fn duration_bound_runs_real_time() {
    let started = std::time::Instant::now();
    let config = BenchConfig {
        prompt: "demo".into(),
        runs: 100_000,
        warmup: 0,
        duration_secs: Some(1),
        ..BenchConfig::default()
    };
    let (handle, progress) = spawn_lane(
        reqwest::Client::new(),
        ProviderSpec::mock(),
        "mock-1".into(),
        config,
    );
    drop(progress);
    let summary = handle.await.unwrap();
    let elapsed = started.elapsed();
    assert!(summary.successful());
    assert!(
        summary.runs.len() >= 2 && summary.runs.len() <= 8,
        "rounds: {}",
        summary.runs.len()
    );
    assert!(elapsed.as_secs() <= 3, "elapsed {elapsed:?}");
}

#[tokio::test]
async fn retries_retry_failed_rounds_before_giving_up() {
    // An unreachable endpoint: every attempt fails fast (connection refused).
    let spec = ProviderSpec::custom("dead", "http://127.0.0.1:9/v1/chat/completions", None, "m");
    let config = BenchConfig {
        prompt: "demo".into(),
        runs: 1,
        warmup: 0,
        timeout_secs: 2,
        retries: 2,
        ..BenchConfig::default()
    };
    let (handle, progress) = spawn_lane(reqwest::Client::new(), spec, "m".into(), config);
    let summary = handle.await.unwrap();
    assert!(!summary.successful());
    let updates: Vec<llmrace::bench::LaneUpdate> = progress.into_iter().collect();
    let retry_notices = updates
        .iter()
        .filter(|u| u.error.as_deref().is_some_and(|e| e.contains("retrying")))
        .count();
    assert_eq!(retry_notices, 2, "expected one notice per retry");
}

#[test]
fn itl_metrics_aggregate_across_runs() {
    let summary = llmrace::bench::LaneSummary {
        name: "x".into(),
        model: "m".into(),
        runs: vec![
            RunMetrics {
                total_ms: 1000.0,
                ttfb_ms: 100.0,
                tokens: 5,
                itl_gaps_ms: vec![10.0, 12.0, 300.0],
                stutters: 1,
                ..RunMetrics::default()
            },
            RunMetrics {
                total_ms: 1000.0,
                ttfb_ms: 100.0,
                tokens: 5,
                itl_gaps_ms: vec![14.0],
                stutters: 0,
                ..RunMetrics::default()
            },
        ],
        error: None,
        prompt: String::new(),
        concurrency: 1,
        variant: None,
        tool_task: None,
    };
    assert_eq!(summary.stutters(), 1);
    let p50 = summary.itl_p50_ms().unwrap();
    let p95 = summary.itl_p95_ms().unwrap();
    assert!((10.0..=14.0).contains(&p50), "p50 {p50}");
    assert!(p95 > 200.0, "p95 {p95}");
}

#[test]
fn itl_absent_when_single_token() {
    let summary = llmrace::bench::LaneSummary {
        name: "x".into(),
        model: "m".into(),
        runs: vec![RunMetrics {
            total_ms: 100.0,
            ttfb_ms: 100.0,
            tokens: 1,
            ..RunMetrics::default()
        }],
        error: None,
        prompt: String::new(),
        concurrency: 1,
        variant: None,
        tool_task: None,
    };
    assert!(summary.itl_p50_ms().is_none());
    assert_eq!(summary.stutters(), 0);
}

#[test]
fn reasoning_burst_throughput_uses_server_window() {
    // gpt-5-style run: thinking until 1652ms, burst delivery in 13ms,
    // usage counts 500 completion tokens, server spent 1398ms processing.
    let m = RunMetrics {
        total_ms: 1665.0,
        ttfb_ms: 1652.0,
        tokens: 500,
        server_ms: Some(1398.0),
        ..RunMetrics::default()
    };
    let tps = m.tokens_per_sec();
    assert!(
        (tps - 500.0 / 1.398).abs() < 1.0,
        "expected ~357 tok/s, got {tps}"
    );
    assert!(tps < 1000.0, "burst artifact not corrected: {tps}");
}

#[test]
fn classic_streaming_throughput_unchanged_by_server_time() {
    // Observed generation window (1000ms) dominates the smaller server
    // processing time (395ms) — throughput stays 100 tok/s.
    let m = RunMetrics {
        total_ms: 1100.0,
        ttfb_ms: 100.0,
        tokens: 100,
        server_ms: Some(395.0),
        ..RunMetrics::default()
    };
    assert!((m.tokens_per_sec() - 100.0).abs() < 1e-9);
}

#[test]
fn lane_throughput_aggregates_effective_windows() {
    let summary = llmrace::bench::LaneSummary {
        name: "x".into(),
        model: "m".into(),
        runs: vec![
            RunMetrics {
                total_ms: 1665.0,
                ttfb_ms: 1652.0,
                tokens: 500,
                server_ms: Some(1398.0),
                ..RunMetrics::default()
            },
            RunMetrics {
                total_ms: 1700.0,
                ttfb_ms: 1660.0,
                tokens: 500,
                server_ms: Some(1400.0),
                ..RunMetrics::default()
            },
        ],
        error: None,
        prompt: String::new(),
        concurrency: 1,
        variant: None,
        tool_task: None,
    };
    let tps = summary.mean_tokens_per_sec();
    assert!((tps - 1000.0 / 2.798).abs() < 1.0, "got {tps}");
}

#[test]
fn no_usage_token_fallback_estimates_from_chars() {
    // Mirrors single_request's fallback: 40 events but 1600 delivered chars
    // (~400 tokens) — the estimate wins over the event count.
    let events = 40u64;
    let chars = 1600u64;
    let tokens = events.max((chars / 4).max(1));
    assert_eq!(tokens, 400);
}

#[test]
fn sse_bare_cr_line_endings_are_separators() {
    // SSE spec allows \r line endings; \r\r is a blank line (separator).
    let mut parser = SseParser::new();
    let blocks = parser.feed(b"data: one\r\rdata: two\r\r").unwrap();
    assert_eq!(blocks, vec!["data: one", "data: two"]);
}

#[test]
fn sse_mixed_line_endings_do_not_split_early() {
    // \r\n inside a single event is a line ending, not a separator;
    // only \r\n\r\n (blank line) terminates the block.
    let mut parser = SseParser::new();
    let blocks = parser
        .feed(b"event: message\r\ndata: payload\r\n\r\ndata: next\n\n")
        .unwrap();
    assert_eq!(
        blocks,
        vec!["event: message\r\ndata: payload", "data: next"]
    );
}

#[test]
fn html_report_renders_lanes_and_bars() {
    let summary = llmrace::bench::LaneSummary {
        name: "mock".into(),
        model: "mock-1".into(),
        runs: vec![RunMetrics {
            total_ms: 300.0,
            ttfb_ms: 40.0,
            tokens: 24,
            ..RunMetrics::default()
        }],
        error: None,
        prompt: "p".into(),
        concurrency: 1,
        variant: None,
        tool_task: None,
    };
    let html = llmrace::report::html_report(std::slice::from_ref(&summary), "demo", 1, 32, &[]);
    assert!(html.contains("<!DOCTYPE html>"));
    assert!(html.contains("mock"));
    assert!(html.contains("<svg"));
    assert!(html.contains("300 ms"));
    assert!(!html.contains("Podium"), "no winner section without scores");

    let scored = vec![llmrace::score::Scored {
        name: "mock".into(),
        model: "mock-1".into(),
        score: 100.0,
    }];
    let with_podium = llmrace::report::html_report(&[summary], "demo", 1, 32, &scored);
    assert!(with_podium.contains("Podium"));
    assert!(with_podium.contains("🥇"));
}

#[test]
fn reasoning_tokens_split_from_visible() {
    let summary = llmrace::bench::LaneSummary {
        name: "gpt5".into(),
        model: "m".into(),
        runs: vec![RunMetrics {
            total_ms: 1665.0,
            ttfb_ms: 1652.0,
            tokens: 500,        // usage: includes hidden reasoning
            visible_tokens: 60, // estimated visible stream
            ..RunMetrics::default()
        }],
        error: None,
        prompt: String::new(),
        concurrency: 1,
        variant: None,
        tool_task: None,
    };
    assert_eq!(summary.reasoning_tokens(), 440);
    assert_eq!(summary.visible_tokens(), 60);

    let classic = llmrace::bench::LaneSummary {
        name: "x".into(),
        model: "m".into(),
        runs: vec![RunMetrics {
            tokens: 100,
            visible_tokens: 100,
            ..RunMetrics::default()
        }],
        error: None,
        prompt: String::new(),
        concurrency: 1,
        variant: None,
        tool_task: None,
    };
    assert_eq!(classic.reasoning_tokens(), 0);
}

#[test]
fn terminal_table_marks_reasoning_lanes() {
    let summary = llmrace::bench::LaneSummary {
        name: "gpt5".into(),
        model: "m".into(),
        runs: vec![RunMetrics {
            total_ms: 1500.0,
            ttfb_ms: 1400.0,
            tokens: 500,
            visible_tokens: 60,
            server_ms: Some(1300.0),
            ..RunMetrics::default()
        }],
        error: None,
        prompt: String::new(),
        concurrency: 1,
        variant: None,
        tool_task: None,
    };
    let table = llmrace::report::terminal_table(&[summary]);
    assert!(table.contains("🧠440"), "{table}");
}

#[test]
fn merge_parses_region_tagged_reports() {
    let eu = r#"{"region":"eu-central","providers":[
        {"provider":"groq","model":"m","successful":true,"mean_total_ms":100.0,
         "p95_total_ms":120.0,"mean_ttfb_ms":20.0,"mean_tokens_per_sec":80.0},
        {"provider":"dead","model":"x","successful":false}]}"#;
    let us = r#"{"providers":[
        {"provider":"groq","model":"m","successful":true,"mean_total_ms":150.0,
         "p95_total_ms":180.0,"mean_ttfb_ms":60.0,"mean_tokens_per_sec":55.0}]}"#;
    let mut lanes = llmrace::merge::parse_report(eu, "eu").unwrap();
    lanes.extend(llmrace::merge::parse_report(us, "probe-us-east").unwrap());
    assert_eq!(lanes.len(), 2); // failed lane skipped
    assert_eq!(lanes[0].region, "eu-central");
    assert_eq!(lanes[1].region, "probe-us-east"); // file-stem fallback

    let table = llmrace::merge::render_table(&lanes);
    assert!(table.contains("groq"), "{table}");
    assert!(table.contains("eu-central"));
    assert!(table.contains("⚡"), "fastest TTFB marked: {table}");
    assert!(table.contains("🚀"), "best throughput marked: {table}");
}

#[test]
fn html_report_escapes_hostile_model_names() {
    let summary = llmrace::bench::LaneSummary {
        name: "custom-lane".into(),
        model: "<script>alert(1)</script>".into(),
        runs: vec![RunMetrics {
            total_ms: 100.0,
            ttfb_ms: 10.0,
            tokens: 5,
            ..RunMetrics::default()
        }],
        error: None,
        prompt: "p <b>bold</b> & 'quotes'".into(),
        concurrency: 1,
        variant: None,
        tool_task: None,
    };
    let html =
        llmrace::report::html_report(std::slice::from_ref(&summary), "prompt <x>", 1, 32, &[]);
    assert!(!html.contains("<script>"), "unescaped script tag");
    assert!(!html.contains("<b>bold</b>"), "unescaped prompt html");
    assert!(html.contains("&lt;script&gt;"), "escaped model missing");
    // failure lanes escape their error text too
    let mut dead = summary;
    dead.error = Some("<img src=x onerror=alert(2)>".into());
    dead.runs.clear();
    let html2 = llmrace::report::html_report(&[dead], "p", 1, 32, &[]);
    assert!(!html2.contains("<img src=x"), "unescaped error html");
}

#[test]
fn csv_escapes_commas_and_quotes() {
    let summary = llmrace::bench::LaneSummary {
        name: "custom".into(),
        model: "model,with,\"quotes\"".into(),
        runs: vec![RunMetrics {
            total_ms: 100.0,
            ttfb_ms: 10.0,
            tokens: 5,
            ..RunMetrics::default()
        }],
        error: None,
        prompt: String::new(),
        concurrency: 1,
        variant: None,
        tool_task: None,
    };
    let csv = llmrace::report::csv(&[summary]);
    let line = csv.lines().nth(1).unwrap_or_default();
    assert!(line.starts_with("custom,"), "{line}");
    assert!(line.ends_with(",1,100.000,10.000,5,55.556"), "{line}");
    assert!(line.contains("\"model,with,\"\"quotes\"\""), "{line}");
}

#[test]
fn stats_edge_branches() {
    use llmrace::stats::{fmt_ms, Sample};
    let empty = Sample::new();
    assert_eq!(empty.mean(), 0.0);
    assert_eq!(empty.percentile(95.0), 0.0);
    assert_eq!(empty.percentiles(&[50.0, 95.0]), vec![0.0, 0.0]);
    assert!(empty.min().is_none() && empty.max().is_none());
    assert!(empty.t_ci95().is_none());
    assert!(empty.outliers_3sigma().is_empty());
    assert_eq!(fmt_ms(0.0), "0");
    assert_eq!(fmt_ms(0.4), "<1");
    assert_eq!(fmt_ms(12.0), "12");

    let single = Sample::from_values(vec![7.0]);
    assert_eq!(single.percentile(0.0), 7.0);
    assert_eq!(single.percentile(100.0), 7.0);
    assert_eq!(single.percentiles(&[10.0, 90.0]), vec![7.0, 7.0]);

    // identical values: zero variance, no outliers
    let flat = Sample::from_values(vec![5.0, 5.0, 5.0]);
    assert_eq!(flat.variance(), 0.0);
    assert!(flat.outliers_3sigma().is_empty());

    // clamping and the two-sided percentile edges
    let s = Sample::from_values(vec![1.0, 2.0, 3.0, 4.0, 5.0]);
    assert_eq!(s.percentile(-10.0), 1.0);
    assert_eq!(s.percentile(1e9), 5.0);
    let ladder = s.percentiles(&[0.0, 50.0, 100.0]);
    assert_eq!(ladder, vec![1.0, 3.0, 5.0]);

    // t-table interpolation between tabulated degrees of freedom (df=12)
    let df12: Vec<f64> = (0..13).map(|i| i as f64).collect();
    let ci = Sample::from_values(df12).t_ci95().unwrap();
    // interpolated t between df=10 (2.228) and df=15 (2.131) ≈ 2.209
    let half = 2.209 * Sample::from_values((0..13).map(|i| i as f64).collect::<Vec<_>>()).stddev()
        / (13.0_f64).sqrt();
    assert!((ci.1 - ci.0 - 2.0 * half).abs() < 0.05, "{ci:?}");

    // obvious 3-sigma outlier detection (needs enough clustered samples
    // that the spike cannot inflate sigma past its own deviation)
    let mut spiky_values: Vec<f64> = (0..20).map(|i| 10.0 + (i as f64 % 5.0) * 0.1).collect();
    spiky_values.push(1000.0);
    let spiky = Sample::from_values(spiky_values);
    assert!(!spiky.outliers_3sigma().is_empty());
}

#[test]
fn race_with_unreachable_endpoint_exits_nonzero() {
    use std::process::Command;
    let out = Command::new(env!("CARGO_BIN_EXE_llmrace"))
        .arg("--endpoint")
        .arg("http://127.0.0.1:9/v1/chat/completions")
        .arg("--provider-name")
        .arg("dead")
        .arg("--no-tui")
        .arg("-n")
        .arg("1")
        .arg("--warmup")
        .arg("0")
        .arg("--timeout")
        .arg("2")
        .arg("--out")
        .arg(
            std::env::temp_dir()
                .join("llmrace-deadrace.md")
                .to_string_lossy()
                .into_owned(),
        )
        .output()
        .expect("spawn llmrace");
    assert!(
        !out.status.success(),
        "total outage must exit non-zero (got {:?})",
        out.status.code()
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("every provider failed"), "{stderr}");
}

#[test]
fn baseline_matches_sweep_variants() {
    // Baseline recorded from a sweep: groq has short and json entries.
    let baseline = llmrace::regression::BaselineReport::from_json_str(
        r#"{"providers":[
            {"provider":"groq","variant":"short","mean_total_ms":100.0,"p95_total_ms":120.0,
             "mean_ttfb_ms":20.0,"mean_tokens_per_sec":80.0},
            {"provider":"groq","variant":"json","mean_total_ms":200.0,"p95_total_ms":240.0,
             "mean_ttfb_ms":40.0,"mean_tokens_per_sec":60.0}]}"#,
    )
    .unwrap();

    let lane = |variant: Option<&str>, total: f64| llmrace::bench::LaneSummary {
        name: "groq".into(),
        model: "m".into(),
        runs: vec![RunMetrics {
            total_ms: total,
            ttfb_ms: 20.0,
            tokens: 10,
            ..RunMetrics::default()
        }],
        error: None,
        prompt: String::new(),
        concurrency: 1,
        variant: variant.map(str::to_string),
        tool_task: None,
    };

    // short lane regressed (100 -> 300 ms), json lane improved (200 -> 100 ms)
    let report = llmrace::regression::evaluate(
        &[lane(Some("short"), 300.0), lane(Some("json"), 100.0)],
        &baseline,
        15.0,
    );
    assert!(report.any_regressed());
    assert_eq!(report.comparisons.len(), 2);
    assert_eq!(report.skipped_providers, Vec::<String>::new());

    // a plain (variant-less) baseline still gates variant lanes as fallback
    let plain = llmrace::regression::BaselineReport::from_json_str(
        r#"{"providers":[{"provider":"groq","mean_total_ms":100.0,"p95_total_ms":120.0,
           "mean_ttfb_ms":20.0,"mean_tokens_per_sec":80.0}]}"#,
    )
    .unwrap();
    let fallback = llmrace::regression::evaluate(&[lane(Some("short"), 300.0)], &plain, 15.0);
    assert!(fallback.any_regressed(), "fallback comparison must run");
}

#[test]
fn markdown_report_notes_unpriced_lanes() {
    let summary = llmrace::bench::LaneSummary {
        name: "custom".into(),
        model: "mystery-model".into(), // not in the pricing table
        runs: vec![RunMetrics {
            total_ms: 100.0,
            ttfb_ms: 10.0,
            tokens: 5,
            ..RunMetrics::default()
        }],
        error: None,
        prompt: String::new(),
        concurrency: 1,
        variant: None,
        tool_task: None,
    };
    let md = llmrace::report::markdown(&[summary], "demo", 1, 32);
    assert!(md.contains("no pricing data for 1 lane(s)"), "{md}");
    assert!(md.contains("--price"), "{md}");
}

#[test]
fn merge_missing_file_reports_which_file() {
    let err = llmrace::merge::load_files(&[
        "/tmp/llmrace-merge-nonexistent-a.json".into(),
        "/tmp/llmrace-merge-nonexistent-b.json".into(),
    ])
    .expect_err("missing files must error");
    assert!(
        err.to_string().contains("llmrace-merge-nonexistent-a.json"),
        "{err}"
    );
}

#[test]
fn thinking_ms_estimate_scales_server_window_by_reasoning_share() {
    let summary = llmrace::bench::LaneSummary {
        name: "openai".into(),
        model: "gpt-5-nano".into(),
        runs: vec![RunMetrics {
            total_ms: 1665.0,
            ttfb_ms: 1652.0,
            tokens: 256,       // usage: reasoning + visible
            visible_tokens: 1, // almost everything was hidden
            server_ms: Some(1398.0),
            ..RunMetrics::default()
        }],
        error: None,
        prompt: String::new(),
        concurrency: 1,
        variant: None,
        tool_task: None,
    };
    let est = summary.thinking_ms_estimate().unwrap();
    assert!((est - 1398.0 * 255.0 / 256.0).abs() < 1e-6, "{est}");
    let json = llmrace::report::json_report(&[summary], "p", 1, 256);
    assert!(json.contains("thinking_ms_estimate"), "{json}");

    // classic models: no estimate
    let classic = llmrace::bench::LaneSummary {
        name: "x".into(),
        model: "m".into(),
        runs: vec![RunMetrics {
            total_ms: 100.0,
            ttfb_ms: 10.0,
            tokens: 50,
            visible_tokens: 50,
            server_ms: Some(90.0),
            ..RunMetrics::default()
        }],
        error: None,
        prompt: String::new(),
        concurrency: 1,
        variant: None,
        tool_task: None,
    };
    assert!(classic.thinking_ms_estimate().is_none());
}

#[test]
fn every_cloud_default_model_has_pricing() {
    // Locks the pricing table against drift: adding a cloud provider
    // without a default-model price row fails here.
    for spec in llmrace::provider::ProviderSpec::registry() {
        if spec.api_key_env.is_some() && *spec.id != *"azure" {
            assert!(
                llmrace::cost::pricing(&spec.id, &spec.default_model).is_some(),
                "no pricing for default model {} / {}",
                spec.id,
                spec.default_model
            );
        }
    }
}

#[test]
fn markdown_reasoning_lane_keeps_tokens_per_sec() {
    // Regression: the reasoning marker used to be passed pre-formatted into
    // the template's `{:.1}` slot, where precision truncated the string to
    // one character — the cell rendered as `*****` and swallowed the value.
    let summary = llmrace::bench::LaneSummary {
        name: "openai".into(),
        model: "gpt-5-nano".into(),
        runs: vec![RunMetrics {
            total_ms: 3177.0,
            ttfb_ms: 3170.0,
            tokens: 511,
            visible_tokens: 1,
            server_ms: Some(1444.0),
            ..RunMetrics::default()
        }],
        error: None,
        prompt: "p".into(),
        concurrency: 1,
        variant: None,
        tool_task: None,
    };
    let md = llmrace::report::markdown(&[summary], "demo", 2, 256);
    let row = md.lines().find(|l| l.contains("gpt-5-nano")).unwrap();
    assert!(row.contains("**353.9**"), "value must render: {row}");
    assert!(row.contains("🧠510"), "marker must render: {row}");
    assert!(!row.contains("*****"), "truncation artifact: {row}");
}
