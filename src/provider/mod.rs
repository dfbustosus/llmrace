//! Provider registry, wire protocols and streaming request plumbing.

mod event_stream;
mod request;
mod sigv4;
mod spec;
mod stream;

pub use event_stream::decode_frame;
pub use request::{
    is_reasoning_model, list_models, split_model_override, stream_completion, CompletionRequest,
};
pub use spec::{AuthKind, ProviderSpec, Wire};
pub use stream::{
    decode_payload, BedrockStream, EventStream, MockStream, ServerTiming, SseStream, StreamEvent,
};
