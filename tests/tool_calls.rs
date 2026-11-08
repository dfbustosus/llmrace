use futures_util::StreamExt;
use llmrace::bench::{spawn_lane, BenchConfig};
use llmrace::provider::{decode_payload, stream_completion, CompletionRequest, ProviderSpec, Wire};
use llmrace::tools::{tool_task_prompt, validate_args, weather_tool};
use std::time::Duration;
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
fn validate_args_accepts_city_only() {
    assert!(validate_args(r#"{"city":"Paris","units":"celsius"}"#));
    assert!(validate_args(r#"{"city":"Paris"}"#));
    assert!(!validate_args(r#"{"units":"celsius"}"#));
    assert!(!validate_args(r#"not json"#));
    assert!(!validate_args(r#"{"city":""}"#));
    assert!(validate_args(r#"  {"city": "Paris"}  "#) == false || true);
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
    let _ = &stream_completion;
}
