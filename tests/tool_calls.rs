use llmrace::bench::{spawn_lane, BenchConfig};
use llmrace::provider::{decode_payload, ProviderSpec, Wire};
use llmrace::tools::{tool_task_prompt, validate_args, weather_tool};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[test]
fn tool_schemas_per_wire() {
    for wire in [
        Wire::OpenAiCompat,
        Wire::Anthropic,
        Wire::Gemini,
        Wire::Cohere,
    ] {
        let schema = weather_tool(wire);
        let text = schema.to_string();
        assert!(text.contains("get_weather"), "{wire:?}");
        assert!(text.contains("city"), "{wire:?}");
    }
}

#[test]
fn exact_match_accepts_a_number_and_a_json_object() {
    assert!(llmrace::tools::answer_matches("  4\n", "4"));
    assert!(llmrace::tools::answer_matches(
        "{\"color\":\"blue\"}",
        "{\"color\":\"blue\"}"
    ));
    assert!(llmrace::tools::answer_matches(
        "{\"b\":1,\"a\":2}",
        "{\"a\":2,\"b\":1}"
    ));
    assert!(!llmrace::tools::answer_matches("the answer is 4", "4"));
    assert!(llmrace::tools::answer_matches_mode(
        "the answer is 4",
        "4",
        llmrace::workload::ExpectMatch::Contains,
    ));
    assert!(!llmrace::tools::answer_matches_mode(
        "the answer is Paris",
        "paris",
        llmrace::workload::ExpectMatch::Contains,
    ));
    assert!(llmrace::tools::answer_matches_mode(
        "  Paris\n",
        "^Paris$",
        llmrace::workload::ExpectMatch::Regex,
    ));
    assert_eq!(
        llmrace::tools::task_ok(true, "weather", r#"{"city":"Paris"}"#, None, ""),
        Some(true)
    );
    assert_eq!(
        llmrace::tools::task_ok(false, "weather", "", Some("4"), "4"),
        Some(true)
    );
    assert_eq!(
        llmrace::tools::task_ok(false, "weather", "", None, "4"),
        None
    );
}

#[test]
fn validate_args_accepts_city_only() {
    assert!(validate_args(r#"{"city":"Paris","units":"celsius"}"#));
    assert!(validate_args(r#"{"city":"Paris"}"#));
    assert!(!validate_args(r#"{"units":"celsius"}"#));
    assert!(!validate_args(r#"not json"#));
    assert!(!validate_args(r#"{"city":""}"#));
}

#[test]
fn openai_tool_call_fragments_decode() {
    let frag = decode_payload(
        r#"{"choices":[{"delta":{"tool_calls":[{"function":{"arguments":"{\"city\":"}}]}}]}"#,
        Wire::OpenAiCompat,
    )
    .unwrap();
    assert_eq!(frag.tool_args.as_deref(), Some(r#"{"city":"#));
    assert!(frag.token.is_none());
}

#[test]
fn anthropic_input_json_delta_decodes() {
    let frag = decode_payload(
        r#"{"type":"content_block_delta","delta":{"type":"input_json_delta","partial_json":"\"Paris\""}}"#,
        Wire::Anthropic,
    )
    .unwrap();
    assert_eq!(frag.tool_args.as_deref(), Some("\"Paris\""));
}

#[test]
fn gemini_function_call_decodes() {
    let frag = decode_payload(
        r#"{"candidates":[{"content":{"parts":[{"functionCall":{"name":"get_weather","args":{"city":"Paris"}}}]}}]}"#,
        Wire::Gemini,
    )
    .unwrap();
    let args = frag.tool_args.unwrap();
    assert!(validate_args(&args));
}

#[tokio::test]
async fn e2e_tool_call_round_tracks_validity() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = Vec::new();
        let mut tmp = [0u8; 2048];
        loop {
            let n = socket.read(&mut tmp).await.unwrap();
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
            if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&buf[..pos]).to_ascii_lowercase();
                if let Some(cl) = headers
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:"))
                    .and_then(|v| v.trim().parse::<usize>().ok())
                {
                    if buf.len() >= pos + 4 + cl {
                        break;
                    }
                } else {
                    break;
                }
            }
        }
        let request = String::from_utf8_lossy(&buf).to_string();
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n\
                  data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"function\":{\"arguments\":\"{\\\"city\\\":\"}}]}}]}\n\n\
                  data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"function\":{\"arguments\":\"\\\"Paris\\\"}\"}}]}}]}\n\n\
                  data: [DONE]\n\n",
            )
            .await
            .unwrap();
        socket.shutdown().await.ok();
        assert!(
            request.contains("get_weather"),
            "tools schema not sent: {request}"
        );
    });

    let mut spec = ProviderSpec::custom(
        "toolup",
        &format!("http://{addr}/v1/chat/completions"),
        None,
        "m",
    );
    spec.auth = llmrace::provider::AuthKind::Bearer;
    let _ = spec;

    let (handle, _progress) = spawn_lane(
        reqwest::Client::new(),
        ProviderSpec::custom(
            "toolup",
            &format!("http://{addr}/v1/chat/completions"),
            None,
            "m",
        ),
        "m".into(),
        BenchConfig {
            prompt: tool_task_prompt().to_string(),
            runs: 1,
            warmup: 0,
            tools: true,
            ..BenchConfig::default()
        },
    );
    let summary = handle.await.unwrap();
    server.await.unwrap();
    assert!(summary.successful(), "{:?}", summary.error);
    assert_eq!(summary.tool_validity(), Some((1, 1)));
    assert!(validate_args(&summary.runs[0].tool_args));
}

#[tokio::test]
async fn plain_runs_have_no_tool_validity() {
    let config = BenchConfig {
        prompt: "hi".into(),
        runs: 1,
        warmup: 0,
        tools: false,
        ..BenchConfig::default()
    };
    let (handle, _p) = spawn_lane(
        reqwest::Client::new(),
        ProviderSpec::mock(),
        "mock-1".into(),
        config,
    );
    let summary = handle.await.unwrap();
    assert_eq!(summary.tool_validity(), None);
}

#[test]
fn tool_task_registry_has_three_tasks_with_valid_schemas() {
    let tasks = llmrace::tools::registry();
    let names: Vec<&str> = tasks.iter().map(|t| t.name).collect();
    assert_eq!(names, vec!["weather", "math", "lookup"]);

    for task in &tasks {
        for wire in [
            Wire::OpenAiCompat,
            Wire::Anthropic,
            Wire::Gemini,
            Wire::Cohere,
        ] {
            let schema = llmrace::tools::tool_schema(task, wire);
            let text = schema.to_string();
            assert!(text.contains(task.function), "{} on {:?}", task.name, wire);
        }
        // validators accept the canonical argument shape
        let valid = match task.name {
            "weather" => r#"{"city":"Paris"}"#,
            "math" => r#"{"expression":"17*23"}"#,
            _ => r#"{"query":"rust 1.0"}"#,
        };
        assert!(
            (task.validate)(valid),
            "{} should accept {}",
            task.name,
            valid
        );
        assert!(
            !(task.validate)("{} not json"),
            "{} should reject junk",
            task.name
        );
    }
}

#[test]
fn math_validator_requires_a_digit() {
    let math = llmrace::tools::find("math").unwrap();
    assert!(
        !(math.validate)(r#"{"expression":"seventeen"}"#),
        "letters only must fail"
    );
    assert!((math.validate)(r#"{"expression":"17 * 23"}"#));
}

#[test]
fn lane_validity_uses_its_own_task_validator() {
    use llmrace::bench::{LaneSummary, RunMetrics};
    let mut lane = LaneSummary {
        name: "x".into(),
        model: "m".into(),
        runs: vec![RunMetrics {
            total_ms: 100.0,
            ttfb_ms: 10.0,
            tool_args: r#"{"expression":"17*23"}"#.into(),
            ..RunMetrics::default()
        }],
        failures: Vec::new(),
        retry_attempts: 0,
        error: None,
        prompt: String::new(),
        concurrency: 1,
        variant: None,
        tool_task: Some("math".into()),
        price_book: None,
    };
    assert_eq!(lane.tool_validity(), Some((1, 1)));

    // same args under the weather task are invalid (no city)
    lane.tool_task = Some("weather".into());
    assert_eq!(lane.tool_validity(), Some((0, 1)));
}
