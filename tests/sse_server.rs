use futures_util::StreamExt;
use llmrace::bench::{spawn_lane, BenchConfig};
use llmrace::provider::{stream_completion, CompletionRequest, ProviderSpec, Wire};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

enum Script {
    Sse {
        events: Vec<String>,
        chunk_size: usize,
    },
    Error {
        status: u16,
        body: String,
    },
    /// Sends one token, then stops sending anything — keeps the connection
    /// open to exercise the mid-stream timeout path.
    Stall,
    /// Usage arrives before any generated content.
    EarlyUsage,
}

async fn spawn_server(script: Script) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let handle = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept");
        let mut buf = Vec::new();
        let mut tmp = [0u8; 1024];
        loop {
            let n = socket.read(&mut tmp).await.expect("read request");
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
            if let Some(header_end) = find_header_end(&buf) {
                let headers = String::from_utf8_lossy(&buf[..header_end]).to_ascii_lowercase();
                if let Some(cl) = content_length(&headers) {
                    if buf.len() >= header_end + 4 + cl {
                        break;
                    }
                } else {
                    break;
                }
            }
        }
        respond(&mut socket, script).await;
        socket.shutdown().await.ok();
    });
    (format!("http://{addr}/v1/chat/completions"), handle)
}

async fn respond(socket: &mut tokio::net::TcpStream, script: Script) {
    match script {
        Script::EarlyUsage => {
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {\"usage\":{\"completion_tokens\":0}}\n\n").await.unwrap();
            tokio::time::sleep(Duration::from_millis(150)).await;
            socket
                .write_all((openai_events().join("\n\n") + "\n\n").as_bytes())
                .await
                .unwrap();
        }
        Script::Sse { events, chunk_size } => {
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
                )
                .await
                .expect("write headers");
            let payload: String = events.join("\n\n") + "\n\n";
            let bytes = payload.as_bytes();
            for chunk in bytes.chunks(chunk_size.max(1)) {
                socket.write_all(chunk).await.expect("write chunk");
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        }
        Script::Error { status, body } => {
            let head = format!(
                "HTTP/1.1 {status} Err\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            socket
                .write_all(head.as_bytes())
                .await
                .expect("write error head");
            socket
                .write_all(body.as_bytes())
                .await
                .expect("write error body");
        }
        Script::Stall => {
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
                )
                .await
                .expect("write headers");
            socket
                .write_all(b"data: {\"choices\":[{\"delta\":{\"content\":\"one\"}}]}\n\n")
                .await
                .expect("write first token");
            std::future::pending::<()>().await;
        }
    }
}

/// Serves `steps` in order, one HTTP request each. Connection: close forces
/// the client to open a new connection for the next round.
async fn spawn_sequence(steps: Vec<Script>) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let handle = tokio::spawn(async move {
        for script in steps {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut buf = Vec::new();
            let mut tmp = [0u8; 2048];
            loop {
                let n = socket.read(&mut tmp).await.expect("read request");
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&tmp[..n]);
                if let Some(header_end) = find_header_end(&buf) {
                    let headers = String::from_utf8_lossy(&buf[..header_end]).to_ascii_lowercase();
                    if let Some(cl) = content_length(&headers) {
                        if buf.len() >= header_end + 4 + cl {
                            break;
                        }
                    } else {
                        break;
                    }
                }
            }
            respond(&mut socket, script).await;
            socket.shutdown().await.ok();
        }
    });
    (format!("http://{addr}/v1/chat/completions"), handle)
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn content_length(headers: &str) -> Option<usize> {
    headers
        .lines()
        .find_map(|l| l.strip_prefix("content-length:"))
        .and_then(|v| v.trim().parse().ok())
}

fn openai_events() -> Vec<String> {
    vec![
        r#"data: {"choices":[{"delta":{"content":"Hel"}}]}"#.into(),
        r#"data: {"choices":[{"delta":{"content":"lo "}}]}"#.into(),
        r#"data: {"choices":[{"delta":{"content":"world"}}]}"#.into(),
        r#"data: {"choices":[{"delta":{"content":"!"}}]}"#.into(),
        r#"data: {"choices":[{"delta":{}}],"usage":{"completion_tokens":4}}"#.into(),
        "data: [DONE]".into(),
    ]
}

/// Serves one SSE response per request and records every request body.
async fn spawn_recorder(steps: usize) -> (String, Arc<std::sync::Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let bodies = Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded = Arc::clone(&bodies);
    tokio::spawn(async move {
        for _ in 0..steps {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut buf = Vec::new();
            let mut tmp = [0u8; 2048];
            let body_start = loop {
                let n = socket.read(&mut tmp).await.expect("read request");
                if n == 0 {
                    break buf.len();
                }
                buf.extend_from_slice(&tmp[..n]);
                if let Some(header_end) = find_header_end(&buf) {
                    let headers = String::from_utf8_lossy(&buf[..header_end]).to_ascii_lowercase();
                    if let Some(cl) = content_length(&headers) {
                        if buf.len() >= header_end + 4 + cl {
                            break header_end + 4;
                        }
                    } else {
                        break header_end + 4;
                    }
                }
            };
            recorded
                .lock()
                .expect("record lock")
                .push(String::from_utf8_lossy(&buf[body_start..]).to_string());
            respond(
                &mut socket,
                Script::Sse {
                    events: openai_events(),
                    chunk_size: 1024,
                },
            )
            .await;
            socket.shutdown().await.ok();
        }
    });
    (format!("http://{addr}/v1/chat/completions"), bodies)
}

#[tokio::test]
async fn warmup_probe_sends_ping_without_priming_the_workload() {
    let (endpoint, bodies) = spawn_recorder(3).await;
    let spec = ProviderSpec::custom("probeup", &endpoint, None, "test-model");
    let config = BenchConfig {
        prompt: "unused".into(),
        runs: 2,
        warmup: 1,
        warmup_mode: llmrace::bench::WarmupMode::ConnectionProbe,
        tools: true,
        tool_task: "weather".into(),
        sampling: llmrace::provider::Sampling {
            system: Some("Be brief.".into()),
            json_output: Some(llmrace::provider::JsonOutput::Object),
            ..llmrace::provider::Sampling::default()
        },
        workload: vec![llmrace::workload::WorkloadPrompt::text("a", "one")],
        ..BenchConfig::default()
    };
    let (handle, progress) = spawn_lane(reqwest::Client::new(), spec, "test-model".into(), config);
    drop(progress);
    let summary = handle.await.expect("lane task");
    assert!(summary.successful(), "error: {:?}", summary.error);
    assert_eq!(summary.runs.len(), 2);
    let bodies = bodies.lock().expect("record lock");
    assert_eq!(bodies.len(), 3);
    let warmup: serde_json::Value = serde_json::from_str(&bodies[0]).unwrap();
    assert_eq!(warmup["messages"][0]["content"], "ping");
    assert_eq!(warmup["max_tokens"], 16);
    assert!(
        warmup.get("tools").is_none(),
        "the probe omits tool schemas: {}",
        bodies[0]
    );
    assert!(
        warmup.get("response_format").is_none(),
        "the probe omits response_format: {}",
        bodies[0]
    );
    assert!(
        !bodies[0].contains("Be brief"),
        "the probe carries no system prompt: {}",
        bodies[0]
    );
    let measured: serde_json::Value = serde_json::from_str(&bodies[1]).unwrap();
    assert_eq!(measured["messages"][0]["content"], "Be brief.");
    assert_eq!(measured["messages"][1]["content"], "one");
    assert!(
        measured.get("tools").is_some(),
        "measured rounds carry configured tools: {}",
        bodies[1]
    );
    assert_eq!(measured["response_format"]["type"], "json_object");
}

#[tokio::test]
async fn warmup_same_prompt_primes_the_first_workload_item() {
    let (endpoint, bodies) = spawn_recorder(2).await;
    let spec = ProviderSpec::custom("warmup", &endpoint, None, "test-model");
    let config = BenchConfig {
        prompt: "unused".into(),
        runs: 1,
        warmup: 1,
        workload: vec![llmrace::workload::WorkloadPrompt::text("a", "one")],
        ..BenchConfig::default()
    };
    let (handle, progress) = spawn_lane(reqwest::Client::new(), spec, "test-model".into(), config);
    drop(progress);
    let summary = handle.await.expect("lane task");
    assert!(summary.successful(), "error: {:?}", summary.error);
    let bodies = bodies.lock().expect("record lock");
    let warmup: serde_json::Value = serde_json::from_str(&bodies[0]).unwrap();
    assert_eq!(
        warmup["messages"][0]["content"], "one",
        "the default warmup sends the first workload item"
    );
}

#[tokio::test]
async fn json_output_reaches_the_wire_and_the_hash_changes() {
    let (endpoint, bodies) = spawn_recorder(1).await;
    let spec = ProviderSpec::custom("jsonup", &endpoint, None, "test-model");
    let config = BenchConfig {
        prompt: "hi".into(),
        runs: 1,
        warmup: 0,
        sampling: llmrace::provider::Sampling {
            json_output: Some(llmrace::provider::JsonOutput::Object),
            ..llmrace::provider::Sampling::default()
        },
        ..BenchConfig::default()
    };
    let (handle, progress) = spawn_lane(
        reqwest::Client::new(),
        spec,
        "test-model".into(),
        config.clone(),
    );
    drop(progress);
    let summary = handle.await.expect("lane task");
    assert!(summary.successful(), "error: {:?}", summary.error);
    let bodies = bodies.lock().expect("record lock");
    let body: serde_json::Value = serde_json::from_str(&bodies[0]).unwrap();
    assert_eq!(body["response_format"]["type"], "json_object");

    let plain = llmrace::report::request_sha256(
        "hi",
        &[],
        &llmrace::provider::Sampling::default(),
        32,
        None,
    );
    let forced = llmrace::report::request_sha256("hi", &[], &config.sampling, 32, None);
    assert_ne!(plain, forced, "forcing JSON output is a different request");
}

#[tokio::test]
async fn full_http_path_streams_and_counts_tokens() {
    let events = openai_events();
    let (endpoint, server) = spawn_server(Script::Sse {
        events,
        chunk_size: 1024,
    })
    .await;
    let spec = ProviderSpec::custom("testup", &endpoint, None, "test-model");

    let (handle, _progress) = spawn_lane(
        reqwest::Client::new(),
        spec,
        "test-model".into(),
        BenchConfig {
            prompt: "hi".into(),
            runs: 1,
            warmup: 0,
            ..BenchConfig::default()
        },
    );
    let summary = handle.await.expect("lane task");
    server.await.expect("server task");
    assert!(summary.successful(), "error: {:?}", summary.error);
    assert_eq!(summary.runs.len(), 1);
    assert_eq!(summary.runs[0].tokens, 4);
    assert!(summary.runs[0].ttfb_ms > 0.0);
    assert!(summary.runs[0].total_ms >= summary.runs[0].ttfb_ms);
}

#[tokio::test]
async fn irregular_chunk_boundaries_survive_the_full_stack() {
    let events = openai_events();
    let (endpoint, server) = spawn_server(Script::Sse {
        events,
        chunk_size: 3,
    })
    .await;
    let spec = ProviderSpec::custom("testup", &endpoint, None, "test-model");

    let mut stream = stream_completion(
        &reqwest::Client::new(),
        &spec,
        &CompletionRequest {
            model: "test-model",
            prompt: "hi",
            max_tokens: 16,
            tools: false,
            tool_task: None,
            sampling: llmrace::provider::Sampling::default(),
            messages: &[],
            tool: None,
        },
    )
    .await
    .expect("stream start");
    let mut tokens = 0u64;
    let mut usage = None;
    while let Some(ev) = stream.next().await {
        let ev = ev.expect("event");
        if ev.token.is_some() {
            tokens += 1;
        }
        if let Some(n) = ev.completion_tokens {
            usage = Some(n);
        }
    }
    server.await.expect("server task");
    assert_eq!(tokens, 4);
    assert_eq!(usage, Some(4));
}

#[tokio::test]
async fn http_error_surfaces_as_lane_error() {
    let body = r#"{"error":{"message":"rate limited","type":"rate_limit"}}"#.to_string();
    let (endpoint, server) = spawn_server(Script::Error { status: 429, body }).await;
    let spec = ProviderSpec::custom("testup", &endpoint, None, "test-model");

    let (handle, _progress) = spawn_lane(
        reqwest::Client::new(),
        spec,
        "test-model".into(),
        BenchConfig {
            prompt: "hi".into(),
            runs: 1,
            warmup: 0,
            ..BenchConfig::default()
        },
    );
    let summary = handle.await.expect("lane task");
    server.await.expect("server task");
    assert!(!summary.successful());
    assert_eq!(summary.failures.len(), 1);
    assert_eq!(
        summary.failures[0].class,
        llmrace::bench::FailureClass::RateLimit
    );
    assert_eq!(summary.runs.len(), 0);
    assert_eq!(summary.retry_attempts, 0);
    let err = summary.error.expect("lane error");
    assert!(err.contains("HTTP 429"), "unexpected error: {err}");
    assert!(
        err.contains("rate limited"),
        "error should include body: {err}"
    );
}

fn rate_limited() -> Script {
    Script::Error {
        status: 429,
        body: r#"{"error":{"message":"rate limited"}}"#.into(),
    }
}

fn one_success() -> Script {
    Script::Sse {
        events: openai_events(),
        chunk_size: 1024,
    }
}

#[tokio::test]
async fn concurrent_failure_is_retained_and_only_failed_requests_retry() {
    for retries in [0, 1] {
        let mut steps = vec![one_success(), rate_limited()];
        if retries > 0 {
            steps.push(one_success());
        }
        let (endpoint, server) = spawn_sequence(steps).await;
        let (handle, _) = spawn_lane(
            reqwest::Client::new(),
            ProviderSpec::custom("partial", &endpoint, None, "m"),
            "m".into(),
            BenchConfig {
                runs: 1,
                warmup: 0,
                concurrency: 2,
                retries,
                ..Default::default()
            },
        );
        let lane = tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .unwrap()
            .unwrap();
        server.await.unwrap();
        assert!(lane.successful());
        assert_eq!(lane.measured_rounds(), 1);
        assert_eq!(lane.measured_requests(), 2);
        assert_eq!(lane.retry_attempts, retries as u64);
        assert_eq!(lane.runs[0].requests.len(), 2);
        assert_eq!(
            lane.runs[0]
                .requests
                .iter()
                .map(|r| r.attempts.len())
                .sum::<usize>(),
            2 + retries
        );
        if retries == 0 {
            assert_eq!(lane.failed_rounds(), 1);
            assert_eq!(lane.failure_rate(), Some(1.0));
            assert_eq!(lane.request_failure_rate(), Some(0.5));
            assert_eq!(lane.runs[0].tokens, 4, "successful sibling is retained");
        } else {
            assert_eq!(lane.failed_rounds(), 0);
            assert_eq!(lane.request_failure_rate(), Some(0.0));
            assert_eq!(lane.runs[0].tokens, 8);
        }
    }
}

#[tokio::test]
async fn usage_only_event_does_not_start_ttft() {
    let (endpoint, server) = spawn_server(Script::EarlyUsage).await;
    let (handle, _) = spawn_lane(
        reqwest::Client::new(),
        ProviderSpec::custom("usage", &endpoint, None, "m"),
        "m".into(),
        BenchConfig {
            runs: 1,
            warmup: 0,
            ..Default::default()
        },
    );
    let lane = handle.await.unwrap();
    server.await.unwrap();
    assert!(lane.successful(), "{:?}", lane.error);
    assert!(lane.mean_ttfb_ms() >= 140.0, "{}", lane.mean_ttfb_ms());
}

#[tokio::test]
async fn truncated_malformed_and_error_streams_are_failures() {
    for suffix in [
        None,
        Some("data: {\"error\":{\"message\":\"overloaded\"}}"),
        Some("data: {broken"),
    ] {
        let mut events =
            vec!["data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}".to_string()];
        events.extend(suffix.map(str::to_string));
        let (endpoint, server) = spawn_server(Script::Sse {
            events,
            chunk_size: 1024,
        })
        .await;
        let (handle, _) = spawn_lane(
            reqwest::Client::new(),
            ProviderSpec::custom("broken", &endpoint, None, "m"),
            "m".into(),
            BenchConfig {
                runs: 1,
                warmup: 0,
                ..Default::default()
            },
        );
        let lane = handle.await.unwrap();
        server.await.unwrap();
        assert!(!lane.successful(), "{suffix:?}");
        assert_eq!(lane.failed_requests(), 1);
        assert_eq!(lane.failures[0].requests[0].attempts.len(), 1);
    }
}

#[tokio::test]
async fn a_fully_failed_prompt_remains_in_json_junit_and_the_prompt_gate() {
    let (endpoint, server) = spawn_sequence(vec![one_success(), rate_limited()]).await;
    let workload = llmrace::workload::parse("{\"id\":\"a\",\"prompt\":\"one\",\"expect\":\"Hello world!\"}\n{\"id\":\"b\",\"prompt\":\"two\",\"expect\":\"Hello world!\"}").unwrap();
    let (handle, _) = spawn_lane(
        reqwest::Client::new(),
        ProviderSpec::custom("partial", &endpoint, None, "m"),
        "m".into(),
        BenchConfig {
            runs: 2,
            warmup: 0,
            workload,
            ..Default::default()
        },
    );
    let lane = handle.await.unwrap();
    server.await.unwrap();
    let rows = lane.prompt_rows();
    let failed = rows.iter().find(|row| row.id == "b").unwrap();
    assert_eq!(failed.rounds, 0);
    assert_eq!(failed.failed_rounds, 1);
    assert_eq!(failed.task_pass, Some((0, 1)));
    let xml = llmrace::report::junit_xml(std::slice::from_ref(&lane));
    assert!(xml.contains("tests=\"2\" failures=\"1\""), "{xml}");
    let json: serde_json::Value = serde_json::from_str(&llmrace::report::json_report(
        std::slice::from_ref(&lane),
        "p",
        2,
        256,
    ))
    .unwrap();
    assert_eq!(json["providers"][0]["failures"][0]["prompt_id"], "b");
    let baseline = llmrace::regression::BaselineReport::from_json_str(r#"{"providers":[{"provider":"partial","model":"m","prompts":[{"id":"b","task_pass_rate":1.0}]}]}"#).unwrap();
    let verdict = llmrace::regression::evaluate(&[lane], &baseline, 15.0).verdict_json(15.0, true);
    assert_eq!(verdict["failures"][0]["prompt_id"], "b");
}

#[tokio::test]
async fn a_failed_measured_round_stays_on_the_lane() {
    let (endpoint, server) =
        spawn_sequence(vec![rate_limited(), rate_limited(), one_success()]).await;
    let spec = ProviderSpec::custom("testup", &endpoint, None, "test-model");
    let (handle, _progress) = spawn_lane(
        reqwest::Client::new(),
        spec,
        "test-model".into(),
        BenchConfig {
            prompt: "hi".into(),
            runs: 2,
            warmup: 1,
            retries: 0,
            ..BenchConfig::default()
        },
    );
    let summary = handle.await.expect("lane task");
    server.await.expect("server task");
    assert!(summary.successful(), "{:?}", summary.error);
    assert!(summary.error.is_none());
    assert_eq!(summary.runs.len(), 1);
    assert_eq!(summary.failures.len(), 1);
    assert_eq!(
        summary.failures[0].class,
        llmrace::bench::FailureClass::RateLimit
    );
    assert_eq!(summary.retry_attempts, 0);
    let rate = summary.failure_rate().expect("measured");
    assert!((rate - 0.5).abs() < 1e-12, "{rate}");
}

#[tokio::test]
async fn a_retry_that_then_succeeds_is_not_a_failed_round() {
    let (endpoint, server) = spawn_sequence(vec![rate_limited(), one_success()]).await;
    let spec = ProviderSpec::custom("testup", &endpoint, None, "test-model");
    let (handle, _progress) = spawn_lane(
        reqwest::Client::new(),
        spec,
        "test-model".into(),
        BenchConfig {
            prompt: "hi".into(),
            runs: 1,
            warmup: 0,
            retries: 1,
            ..BenchConfig::default()
        },
    );
    let summary = handle.await.expect("lane task");
    server.await.expect("server task");
    assert!(summary.successful(), "{:?}", summary.error);
    assert!(summary.failures.is_empty());
    assert_eq!(summary.runs.len(), 1);
    assert_eq!(summary.retry_attempts, 1);
    assert_eq!(summary.failure_rate(), Some(0.0));
}

#[test]
fn wire_of_custom_provider_is_openai_compat() {
    let spec = ProviderSpec::custom("x", "http://localhost:9/v1/chat/completions", None, "m");
    assert_eq!(spec.wire, Wire::OpenAiCompat);
}

#[tokio::test]
async fn azure_style_auth_sends_api_key_header_not_bearer() {
    let spec_arc = Arc::new(std::sync::atomic::AtomicI64::new(0));
    let captured = Arc::new(std::sync::Mutex::new(String::new()));

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let cap_clone = Arc::clone(&captured);
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept");
        let mut buf = Vec::new();
        let mut tmp = [0u8; 2048];
        let head_end = loop {
            let n = socket.read(&mut tmp).await.expect("read");
            buf.extend_from_slice(&tmp[..n]);
            if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break pos;
            }
        };
        *cap_clone.lock().unwrap() = String::from_utf8_lossy(&buf[..head_end]).to_string();
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n\
                  data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n",
            )
            .await
            .unwrap();
        socket.shutdown().await.ok();
    });
    let _ = spec_arc;

    std::env::set_var("LLMRACE_TEST_AZURE_KEY", "super-secret");
    let mut spec = llmrace::provider::ProviderSpec::custom(
        "azure-test",
        &format!("http://{addr}/openai/deployments/d/chat/completions"),
        Some("LLMRACE_TEST_AZURE_KEY"),
        "d",
    );
    spec.auth = llmrace::provider::AuthKind::ApiKeyHeader;

    let mut stream = stream_completion(
        &reqwest::Client::new(),
        &spec,
        &CompletionRequest {
            model: "d",
            prompt: "hi",
            max_tokens: 8,
            tools: false,
            tool_task: None,
            sampling: llmrace::provider::Sampling::default(),
            messages: &[],
            tool: None,
        },
    )
    .await
    .expect("stream");
    use futures_util::StreamExt;
    while stream.next().await.is_some() {}
    server.await.unwrap();
    std::env::remove_var("LLMRACE_TEST_AZURE_KEY");

    let headers = captured.lock().unwrap().clone();
    assert!(
        headers
            .to_ascii_lowercase()
            .contains("api-key: super-secret"),
        "api-key header missing: {headers}"
    );
    assert!(
        !headers
            .to_ascii_lowercase()
            .contains("authorization: bearer"),
        "bearer header should not be sent: {headers}"
    );
}

#[tokio::test]
async fn ollama_model_not_found_gets_actionable_hint() {
    let body =
        r#"{"error":{"message":"model 'llama3.2' not found, try pulling it first"}}"#.to_string();
    let (endpoint, server) = spawn_server(Script::Error { status: 404, body }).await;
    let spec = llmrace::provider::ProviderSpec::custom("ollama", &endpoint, None, "llama3.2");

    let (handle, _progress) = spawn_lane(
        reqwest::Client::new(),
        spec,
        "llama3.2".into(),
        BenchConfig {
            prompt: "hi".into(),
            runs: 1,
            warmup: 0,
            ..BenchConfig::default()
        },
    );
    let summary = handle.await.expect("lane task");
    server.await.expect("server task");
    let err = summary.error.expect("expected lane error");
    assert!(err.contains("HTTP 404"), "{err}");
    assert!(err.contains("ollama pull"), "hint missing: {err}");
    assert!(err.contains("-m <model>"), "{err}");
}

#[tokio::test]
async fn gpt5_models_send_max_completion_tokens() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let captured = Arc::new(std::sync::Mutex::new(String::new()));
    let cap = Arc::clone(&captured);
    let server = tokio::spawn(async move {
        for _ in 0..3 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = Vec::new();
            let mut tmp = [0u8; 4096];
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
            *cap.lock().unwrap() = String::from_utf8_lossy(&buf).to_string();
            socket
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n\
                  data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n",
            )
            .await
            .unwrap();
            socket.shutdown().await.ok();
        }
    });

    for (model, expect_key, reject_key) in [
        ("gpt-5-nano", "max_completion_tokens", "max_tokens"),
        ("openai/gpt-5", "max_completion_tokens", "max_tokens"),
        ("gpt-4o-mini", "max_tokens", "max_completion_tokens"),
    ] {
        let spec = ProviderSpec::custom(
            "t",
            &format!("http://{addr}/v1/chat/completions"),
            None,
            model,
        );
        let mut stream = stream_completion(
            &reqwest::Client::new(),
            &spec,
            &CompletionRequest {
                model,
                prompt: "hi",
                max_tokens: 8,
                tools: false,
                tool_task: None,
                sampling: llmrace::provider::Sampling::default(),
                messages: &[],
                tool: None,
            },
        )
        .await
        .unwrap();
        use futures_util::StreamExt;
        while stream.next().await.is_some() {}
        let body = captured.lock().unwrap().clone();
        assert!(
            body.contains(&format!("\"{expect_key}\"")),
            "{model}: {body}"
        );
        assert!(
            !body.contains(&format!("\"{reject_key}\"")),
            "{model}: {body}"
        );
        *captured.lock().unwrap() = String::new();
        let _ = tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    server.abort();
}

#[tokio::test]
async fn mid_stream_timeout_names_model_and_window() {
    let (endpoint, _server) = spawn_server(Script::Stall).await;
    let spec = ProviderSpec::custom("stalled", &endpoint, None, "stall-model");
    let config = BenchConfig {
        prompt: "demo".into(),
        runs: 1,
        warmup: 0,
        timeout_secs: 1,
        ..BenchConfig::default()
    };
    let (handle, _progress) =
        spawn_lane(reqwest::Client::new(), spec, "stall-model".into(), config);
    let summary = handle.await.unwrap();
    let err = summary.error.expect("run must time out");
    assert!(err.contains("timed out after"), "{err}");
    assert!(err.contains("stall-model"), "{err}");
    assert!(err.contains("1 tokens received"), "{err}");
}

#[tokio::test]
async fn list_models_decodes_openai_shape() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = Vec::new();
        let mut tmp = [0u8; 1024];
        loop {
            let n = socket.read(&mut tmp).await.unwrap();
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
            if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&buf[..pos]).to_ascii_lowercase();
                assert!(
                    head.starts_with("get /v1/models"),
                    "must call /v1/models: {head}"
                );
                break;
            }
        }
        let body = r#"{"data":[{"id":"b-model"},{"id":"a-model"}]}"#;
        socket
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .as_bytes(),
            )
            .await
            .unwrap();
    });
    let spec = ProviderSpec::custom(
        "testy",
        &format!("http://{addr}/v1/chat/completions"),
        None,
        "m",
    );
    let models = llmrace::provider::list_models(&reqwest::Client::new(), &spec)
        .await
        .unwrap();
    server.abort();
    assert_eq!(models, vec!["a-model", "b-model"], "sorted and deduped");
}

#[tokio::test]
async fn list_models_gemini_shape_and_mock_error() {
    // mock wire: clean error, no HTTP
    let err = llmrace::provider::list_models(&reqwest::Client::new(), &ProviderSpec::mock())
        .await
        .expect_err("mock has no models");
    assert!(err.to_string().contains("no model listing"), "{err}");
}

#[tokio::test]
async fn model_not_found_errors_point_at_discovery() {
    // OpenAI-style 404 model error gets the --models hint
    let body = r#"{"error":{"message":"The model 'gpt-nope' does not exist or you do not have access to it.","type":"invalid_request_error","param":null,"code":"model_not_found"}}"#.to_string();
    let (endpoint, _server) = spawn_server(Script::Error { status: 404, body }).await;
    let spec = ProviderSpec::custom("openai-ish", &endpoint, None, "gpt-nope");
    let config = BenchConfig {
        prompt: "demo".into(),
        runs: 1,
        warmup: 0,
        ..BenchConfig::default()
    };
    let (handle, _p) = spawn_lane(reqwest::Client::new(), spec, "gpt-nope".into(), config);
    let summary = handle.await.unwrap();
    let err = summary.error.expect("must fail");
    assert!(
        err.to_string().contains("llmrace --models openai-ish"),
        "hint missing: {err}"
    );
}

#[tokio::test]
async fn permanent_client_errors_are_not_retried() {
    let (endpoint, server) = spawn_server(Script::Error {
        status: 400,
        body: r#"{"error":"bad request"}"#.into(),
    })
    .await;
    let spec = ProviderSpec::custom("bad", &endpoint, None, "nope");
    let started = std::time::Instant::now();
    let (handle, _progress) = spawn_lane(
        reqwest::Client::new(),
        spec,
        "nope".into(),
        BenchConfig {
            prompt: "hi".into(),
            runs: 1,
            warmup: 0,
            retries: 5,
            timeout_secs: 5,
            ..BenchConfig::default()
        },
    );
    let summary = handle.await.expect("lane task");
    let elapsed = started.elapsed();
    server.abort();
    assert!(!summary.successful(), "{summary:?}");
    assert!(
        elapsed < Duration::from_millis(1500),
        "a 400 was retried; elapsed {elapsed:?}"
    );
}
