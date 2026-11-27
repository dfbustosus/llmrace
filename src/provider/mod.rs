//! Provider registry, wire protocols and streaming request plumbing.

mod event_stream;
mod gcp_oauth;
mod request;
mod sigv4;

pub(crate) use sigv4::sha256_hex;
mod spec;
mod stream;

pub use event_stream::decode_frame;
/// Quick check: are AWS credentials present in the environment?
pub fn sigv4_credential_check() -> Option<()> {
    if std::env::var("AWS_ACCESS_KEY_ID").is_ok() && std::env::var("AWS_SECRET_ACCESS_KEY").is_ok()
    {
        Some(())
    } else {
        None
    }
}

pub use request::{
    is_reasoning_model, list_models, split_model_override, stream_completion, CompletionRequest,
    JsonOutput, Sampling,
};

/// HTTP client tuned for a fair streaming race.
///
/// The connect timeout is capped at 10s so a dead endpoint fails before
/// the request budget is spent. The idle pool is sized to `concurrency`
/// so the warmup round actually reuses the connection the measured rounds
/// use, and TCP plus HTTP/2 keepalives stop a quiet `--watch` interval
/// from paying a fresh handshake on every round. HTTP/2 is negotiated
/// when the server offers it (one connection, multiplexed streams); HTTP/1.1
/// servers such as a local daemon stay on a pooled connection per stream.
pub fn benchmark_client(
    timeout: std::time::Duration,
    concurrency: usize,
) -> crate::error::Result<reqwest::Client> {
    let timeout = timeout.max(std::time::Duration::from_secs(1));
    let pool = concurrency.clamp(1, 64);
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(std::time::Duration::from_secs(10).min(timeout))
        .timeout(timeout)
        .pool_idle_timeout(std::time::Duration::from_secs(90))
        .pool_max_idle_per_host(pool)
        .tcp_keepalive(std::time::Duration::from_secs(30))
        .http2_keep_alive_interval(Some(std::time::Duration::from_secs(30)))
        .http2_keep_alive_timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| crate::error::Error::Config(format!("failed to build http client: {e}")))
}
pub use spec::{AuthKind, ProviderSpec, Wire};
pub use stream::{
    decode_payload, BedrockStream, EventStream, MockStream, ServerTiming, SseStream, StreamEvent,
};
