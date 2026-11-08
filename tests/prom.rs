use llmrace::bench::{spawn_lane, BenchConfig};
use llmrace::prom::{push, render_metrics};
use llmrace::provider::ProviderSpec;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[tokio::test]
async fn metrics_render_in_prometheus_text_format() {
    let config = BenchConfig {
        prompt: "demo".into(),
        runs: 2,
        warmup: 0,
        ..BenchConfig::default()
    };
    let (handle, _p) = spawn_lane(
        reqwest::Client::new(),
        ProviderSpec::mock(),
        "mock-1".into(),
        config,
    );
    let summary = handle.await.unwrap();
    let metrics = render_metrics(std::slice::from_ref(&summary));
    assert!(metrics.contains("llmrace_ttfb_ms{provider=\"mock\",model=\"mock-1\"}"));
    assert!(metrics.contains("llmrace_tokens_per_second{"));
    assert!(metrics.contains("llmrace_latency_p95_ms{"));
}

#[tokio::test]
async fn push_sends_put_with_text_body_and_handles_errors() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let captured = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let cap = std::sync::Arc::clone(&captured);
    let server = tokio::spawn(async move {
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
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        socket.shutdown().await.ok();
    });

    let config = BenchConfig {
        prompt: "demo".into(),
        runs: 1,
        warmup: 0,
        ..BenchConfig::default()
    };
    let (handle, _p) = spawn_lane(
        reqwest::Client::new(),
        ProviderSpec::mock(),
        "mock-1".into(),
        config,
    );
    let summary = handle.await.unwrap();

    let url = format!("http://{addr}/metrics/job/llmrace");
    push(
        &reqwest::Client::new(),
        &url,
        std::slice::from_ref(&summary),
    )
    .await
    .unwrap();
    server.await.unwrap();

    let raw = captured.lock().unwrap().clone();
    let lower = raw.to_ascii_lowercase();
    assert!(lower.starts_with("put /metrics/job/llmrace"), "{raw}");
    assert!(lower.contains("content-type: text/plain"), "{raw}");
    assert!(raw.contains("llmrace_ttfb_ms{provider=\"mock\""), "{raw}");

    let err = push(
        &reqwest::Client::new(),
        &format!("http://{addr}/nothing-listening-anymore"),
        &[],
    )
    .await;
    assert!(err.is_err());
}
