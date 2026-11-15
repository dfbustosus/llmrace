//! Provider registry, wire protocols and streaming request plumbing.

mod request;
mod spec;
mod stream;

pub use request::{
    is_reasoning_model, list_models, split_model_override, stream_completion, CompletionRequest,
};
pub use spec::{AuthKind, ProviderSpec, Wire};
pub use stream::{decode_payload, EventStream, MockStream, ServerTiming, SseStream, StreamEvent};
