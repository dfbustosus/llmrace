//! Provider registry, wire protocols and streaming request plumbing.

mod event_stream;
mod gcp_oauth;
mod request;
mod sigv4;
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
};
pub use spec::{AuthKind, ProviderSpec, Wire};
pub use stream::{
    decode_payload, BedrockStream, EventStream, MockStream, ServerTiming, SseStream, StreamEvent,
};
