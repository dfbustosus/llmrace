//! Streaming event types and decoders: [`EventStream`], the HTTP SSE
//! stream, per-wire payload decoding and the deterministic mock stream.

use super::spec::Wire;
use crate::error::{Error, Result};
use crate::sse::parse_data_events;
use std::pin::Pin;
use std::task::Poll;
use std::time::Duration;

/// One decoded streaming event.
#[derive(Debug, Clone, Default)]
pub struct StreamEvent {
    /// A generated text fragment, when this event carries one.
    pub token: Option<String>,
    /// A tool-call argument JSON fragment, when this event carries one.
    pub tool_args: Option<String>,
    /// Provider-reported completion token count, when available.
    pub completion_tokens: Option<u64>,
    /// Provider-reported prompt token count, when this event carries usage.
    ///
    /// This is the full input count: uncached tokens plus cache reads and
    /// cache writes. It includes a system prompt and tool schemas. Anthropic
    /// and Bedrock report those buckets separately, and the decoder adds
    /// them up so `prompt_tokens` means the same thing on every wire.
    /// `None` on ordinary token deltas.
    pub prompt_tokens: Option<u64>,
    /// Tokens billed at the cache-read rate. A subset of [`Self::prompt_tokens`].
    pub cache_read_tokens: Option<u64>,
    /// Tokens billed at the short cache-write rate (Anthropic's 5-minute
    /// write, or the whole write when the payload does not split TTLs).
    /// A subset of [`Self::prompt_tokens`], disjoint from the read bucket.
    pub cache_write_tokens: Option<u64>,
    /// Tokens billed at the 1-hour cache-write rate, when the payload
    /// distinguishes that TTL. A subset of [`Self::prompt_tokens`].
    pub cache_write_1h_tokens: Option<u64>,
    /// Provider-reported reasoning tokens (`thoughtsTokenCount`, OpenAI
    /// `reasoning_tokens`). `None` when the provider sent no count.
    pub reasoning_tokens: Option<u64>,
    /// `true` when [`Self::reasoning_tokens`] is billed on top of
    /// [`Self::completion_tokens`]. Gemini thoughts are separate.
    /// OpenAI `completion_tokens` already includes reasoning.
    pub reasoning_billed_separately: bool,
    /// `true` when hidden reasoning/thinking activity was present in this frame;
    /// by itself never starts the TTFT clock.
    pub reasoning_seen: bool,
    /// Provider-reported end-to-end server duration (prefill + generation) in
    /// milliseconds when available (e.g., Bedrock `metadata.metrics.latencyMs`),
    /// distinct from pre-stream [`ServerTiming::processing_ms`].
    pub server_total_ms: Option<f64>,
}

impl StreamEvent {
    fn has_payload(&self) -> bool {
        self.token.is_some()
            || self.tool_args.is_some()
            || self.completion_tokens.is_some()
            || self.prompt_tokens.is_some()
            || self.cache_read_tokens.is_some()
            || self.cache_write_tokens.is_some()
            || self.cache_write_1h_tokens.is_some()
            || self.reasoning_tokens.is_some()
            || self.server_total_ms.is_some()
    }

    fn is_reasoning_only(&self) -> bool {
        self.reasoning_seen && !self.has_payload()
    }
}

/// Input-side usage, normalized so every cache bucket is a subset of
/// `prompt_tokens`.
struct InputUsage {
    prompt_tokens: Option<u64>,
    cache_read_tokens: Option<u64>,
    cache_write_tokens: Option<u64>,
    cache_write_1h_tokens: Option<u64>,
}

impl InputUsage {
    fn empty() -> Self {
        InputUsage {
            prompt_tokens: None,
            cache_read_tokens: None,
            cache_write_tokens: None,
            cache_write_1h_tokens: None,
        }
    }

    fn is_empty(&self) -> bool {
        self.prompt_tokens.is_none()
            && self.cache_read_tokens.is_none()
            && self.cache_write_tokens.is_none()
            && self.cache_write_1h_tokens.is_none()
    }

    fn into_event(
        self,
        token: Option<String>,
        tool_args: Option<String>,
        completion_tokens: Option<u64>,
    ) -> StreamEvent {
        StreamEvent {
            token,
            tool_args,
            completion_tokens,
            prompt_tokens: self.prompt_tokens,
            cache_read_tokens: self.cache_read_tokens,
            cache_write_tokens: self.cache_write_tokens,
            cache_write_1h_tokens: self.cache_write_1h_tokens,
            reasoning_tokens: None,
            reasoning_billed_separately: false,
            reasoning_seen: false,
            server_total_ms: None,
        }
    }
}

fn json_u64(value: Option<&serde_json::Value>) -> Option<u64> {
    value.and_then(serde_json::Value::as_u64)
}

fn field_u64(obj: &serde_json::Value, names: &[&str]) -> Option<u64> {
    names.iter().find_map(|name| json_u64(obj.get(*name)))
}

/// OpenAI's `prompt_tokens` already includes `cached_tokens`.
/// DeepSeek reports the same cache-hit bucket as `prompt_cache_hit_tokens`.
fn openai_input(v: &serde_json::Value) -> InputUsage {
    InputUsage {
        prompt_tokens: json_u64(v.pointer("/usage/prompt_tokens")),
        cache_read_tokens: json_u64(v.pointer("/usage/prompt_tokens_details/cached_tokens"))
            .or_else(|| json_u64(v.pointer("/usage/prompt_cache_hit_tokens"))),
        cache_write_tokens: None,
        cache_write_1h_tokens: None,
    }
}

/// Anthropic's `input_tokens` excludes cache reads and cache writes.
/// The split ephemeral fields win over the combined creation count so a
/// 1-hour write is not also billed as a 5-minute write.
fn anthropic_usage(usage: &serde_json::Value) -> InputUsage {
    let uncached = field_u64(usage, &["input_tokens"]);
    let cache_read = field_u64(usage, &["cache_read_input_tokens"]);
    let write_5m = json_u64(usage.pointer("/cache_creation/ephemeral_5m_input_tokens"));
    let write_1h = json_u64(usage.pointer("/cache_creation/ephemeral_1h_input_tokens"));
    let write_total = field_u64(usage, &["cache_creation_input_tokens"]);
    let (cache_write, cache_write_1h) = if write_5m.is_some() || write_1h.is_some() {
        (write_5m, write_1h)
    } else {
        (write_total, None)
    };
    summed_input(uncached, cache_read, cache_write, cache_write_1h)
}

fn anthropic_input(v: &serde_json::Value) -> InputUsage {
    let from_message = v.pointer("/message/usage").map(anthropic_usage);
    let from_usage = v.get("usage").map(anthropic_usage);
    match (from_message, from_usage) {
        (Some(message), Some(usage)) => {
            if message.prompt_tokens.is_some() {
                message
            } else {
                usage
            }
        }
        (Some(message), None) => message,
        (None, Some(usage)) => usage,
        (None, None) => InputUsage::empty(),
    }
}

/// Gemini's `promptTokenCount` already includes `cachedContentTokenCount`.
fn gemini_input(v: &serde_json::Value) -> InputUsage {
    InputUsage {
        prompt_tokens: json_u64(v.pointer("/usageMetadata/promptTokenCount")),
        cache_read_tokens: json_u64(v.pointer("/usageMetadata/cachedContentTokenCount")),
        cache_write_tokens: None,
        cache_write_1h_tokens: None,
    }
}

/// Bedrock Converse follows Anthropic: `inputTokens` excludes the cache
/// buckets. Cache fields are added into the prompt total only when present.
fn bedrock_input(usage: &serde_json::Value) -> InputUsage {
    let uncached = field_u64(usage, &["inputTokens", "input_tokens"]);
    let cache_read = field_u64(usage, &["cacheReadInputTokens", "cache_read_input_tokens"]);
    let cache_write = field_u64(
        usage,
        &["cacheWriteInputTokens", "cache_write_input_tokens"],
    );
    summed_input(uncached, cache_read, cache_write, None)
}

fn bedrock_metadata_event(v: &serde_json::Value) -> Option<StreamEvent> {
    let (input, completion) = match v.get("usage") {
        Some(usage) => (
            bedrock_input(usage),
            field_u64(usage, &["outputTokens", "output_tokens"]),
        ),
        None => (InputUsage::empty(), None),
    };
    let server_total_ms = v
        .pointer("/metrics/latencyMs")
        .and_then(serde_json::Value::as_f64)
        .filter(|ms| ms.is_finite() && (0.0..=86_400_000.0).contains(ms));
    if completion.is_none() && input.is_empty() && server_total_ms.is_none() {
        return None;
    }
    let mut event = input.into_event(None, None, completion);
    event.server_total_ms = server_total_ms;
    Some(event)
}

fn summed_input(
    uncached: Option<u64>,
    cache_read: Option<u64>,
    cache_write: Option<u64>,
    cache_write_1h: Option<u64>,
) -> InputUsage {
    let any = uncached.is_some()
        || cache_read.is_some()
        || cache_write.is_some()
        || cache_write_1h.is_some();
    let prompt_tokens = if any {
        Some(
            uncached
                .unwrap_or(0)
                .saturating_add(cache_read.unwrap_or(0))
                .saturating_add(cache_write.unwrap_or(0))
                .saturating_add(cache_write_1h.unwrap_or(0)),
        )
    } else {
        None
    };
    InputUsage {
        prompt_tokens,
        cache_read_tokens: cache_read,
        cache_write_tokens: cache_write,
        cache_write_1h_tokens: cache_write_1h,
    }
}

/// Provider-reported timing and identity metadata from response headers.
#[derive(Debug, Clone, Default)]
pub struct ServerTiming {
    /// Server-side processing time in ms (OpenAI: `openai-processing-ms`).
    pub processing_ms: Option<f64>,
    /// Provider request ID (`x-request-id`).
    pub request_id: Option<String>,
    /// Remaining rate-limit requests (`x-ratelimit-remaining-requests`).
    pub ratelimit_remaining: Option<String>,
}

/// The stream returned by [`super::stream_completion`].
///
/// A named enum instead of a boxed trait object so that common cases avoid
/// virtual dispatch and the type is `Unpin + Send` by construction.
pub enum EventStream {
    /// Deterministic mock provider stream.
    Mock(MockStream),
    /// HTTP SSE stream.
    Http(SseStream),
    /// Amazon Bedrock binary event-stream.
    Bedrock(BedrockStream),
}

impl EventStream {
    /// Returns provider-reported timing and identity metadata captured
    /// from the HTTP response headers, when available.
    pub fn server_timing(&self) -> ServerTiming {
        match self {
            EventStream::Http(s) => s.server_timing(),
            EventStream::Mock(_) | EventStream::Bedrock(_) => ServerTiming::default(),
        }
    }
}

impl futures_util::Stream for EventStream {
    type Item = Result<StreamEvent>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        match &mut *self {
            EventStream::Mock(s) => Pin::new(s).poll_next(cx),
            EventStream::Http(s) => Pin::new(s).poll_next(cx),
            EventStream::Bedrock(s) => Pin::new(s).poll_next(cx),
        }
    }
}

/// A live HTTP SSE body decoded into [`StreamEvent`]s.
pub struct SseStream {
    stream: Pin<Box<dyn futures_util::Stream<Item = reqwest::Result<bytes::Bytes>> + Send>>,
    parser: crate::sse::SseParser,
    wire: Wire,
    pending: std::collections::VecDeque<Result<StreamEvent>>,
    upstream_done: bool,
    completed: bool,
    reasoning_emitted: bool,
    timing: ServerTiming,
}

impl SseStream {
    pub(super) fn new(resp: reqwest::Response, wire: Wire) -> Self {
        let header = |name: &str| {
            resp.headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
        };
        let processing_ms = header("openai-processing-ms")
            .or_else(|| header("x-processing-time-ms"))
            .and_then(|v| v.trim().parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v >= 0.0);
        let timing = ServerTiming {
            processing_ms,
            request_id: header("x-request-id"),
            ratelimit_remaining: header("x-ratelimit-remaining-requests"),
        };
        SseStream {
            stream: Box::pin(resp.bytes_stream()),
            parser: crate::sse::SseParser::new(),
            wire,
            pending: std::collections::VecDeque::new(),
            upstream_done: false,
            completed: false,
            reasoning_emitted: false,
            timing,
        }
    }

    /// Returns captured response-header timing metadata.
    pub fn server_timing(&self) -> ServerTiming {
        self.timing.clone()
    }

    fn enqueue_block(&mut self, block: &str) {
        let data_lines = parse_data_events(block);
        let payload: std::borrow::Cow<'_, str> = match data_lines.as_slice() {
            [] => return,
            [one] => std::borrow::Cow::Borrowed(*one),
            many => std::borrow::Cow::Owned(many.join("\n")),
        };
        if payload.is_empty() || self.upstream_done {
            return;
        }
        if payload == "[DONE]" {
            self.completed = true;
            self.upstream_done = true;
            return;
        }
        let value: serde_json::Value = match serde_json::from_str(&payload) {
            Ok(value) => value,
            Err(error) => {
                self.pending.push_back(Err(Error::Protocol(format!(
                    "invalid streaming JSON: {error}"
                ))));
                self.upstream_done = true;
                return;
            }
        };
        let kind = value.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if value.get("error").is_some_and(|e| !e.is_null())
            || matches!(kind, "error" | "response.failed")
        {
            self.pending.push_back(Err(Error::Stream(format!(
                "provider stream error: {}",
                crate::util::truncate(&value.to_string(), 300)
            ))));
            self.upstream_done = true;
            return;
        }
        self.completed |= match self.wire {
            Wire::OpenAiCompat => value
                .pointer("/choices/0/finish_reason")
                .is_some_and(|v| v.is_string()),
            Wire::Anthropic => kind == "message_stop",
            Wire::Gemini | Wire::Vertex => value
                .pointer("/candidates/0/finishReason")
                .is_some_and(|v| v.is_string()),
            Wire::Cohere => {
                kind == "message-end"
                    || value.get("event_type").and_then(|v| v.as_str()) == Some("stream-end")
            }
            Wire::OpenAiResponses => matches!(kind, "response.completed" | "response.incomplete"),
            _ => false,
        };
        if let Some(event) = decode_value(&value, self.wire) {
            if event.has_payload() || event.reasoning_seen {
                if event.is_reasoning_only() {
                    if self.reasoning_emitted {
                        return;
                    }
                    self.reasoning_emitted = true;
                }
                self.pending.push_back(Ok(event));
            }
        }
    }
}

impl futures_util::Stream for SseStream {
    type Item = Result<StreamEvent>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        loop {
            if let Some(event) = self.pending.pop_front() {
                return Poll::Ready(Some(event));
            }
            if self.upstream_done {
                return Poll::Ready(None);
            }
            match futures_util::StreamExt::poll_next_unpin(&mut self.stream, cx) {
                Poll::Ready(Some(Ok(bytes))) => match self.parser.feed(&bytes) {
                    Ok(blocks) => {
                        for block in blocks {
                            self.enqueue_block(&block);
                        }
                    }
                    Err(e) => {
                        self.upstream_done = true;
                        return Poll::Ready(Some(Err(e)));
                    }
                },
                Poll::Ready(Some(Err(e))) => {
                    self.upstream_done = true;
                    return Poll::Ready(Some(Err(Error::Stream(
                        super::request::format_reqwest_error(None, e),
                    ))));
                }
                Poll::Ready(None) => {
                    for block in self.parser.finish() {
                        self.enqueue_block(&block);
                    }
                    if !self.completed && !self.upstream_done {
                        self.pending.push_back(Err(Error::Protocol(
                            "response stream ended without a completion marker".into(),
                        )));
                    }
                    self.upstream_done = true;
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

/// Decodes one SSE `data:` payload into a [`StreamEvent`] for the given wire.
///
/// Returns `None` for control payloads (`[DONE]`, keepalives) or unrecognized
/// events.
pub fn decode_payload(payload: &str, wire: Wire) -> Option<StreamEvent> {
    if payload == "[DONE]" {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(payload).ok()?;
    decode_value(&v, wire)
}

fn decode_value(v: &serde_json::Value, wire: Wire) -> Option<StreamEvent> {
    match wire {
        Wire::OpenAiCompat => {
            let token = v
                .pointer("/choices/0/delta/content")
                .and_then(|c| c.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            let tool_args = v
                .pointer("/choices/0/delta/tool_calls/0/function/arguments")
                .and_then(|c| c.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            let reasoning_seen = v
                .pointer("/choices/0/delta/reasoning_content")
                .and_then(|c| c.as_str())
                .is_some_and(|s| !s.is_empty())
                || v.pointer("/choices/0/delta/reasoning")
                    .and_then(|c| c.as_str())
                    .is_some_and(|s| !s.is_empty());
            let usage = json_u64(v.pointer("/usage/completion_tokens"));
            let mut event = openai_input(v).into_event(token, tool_args, usage);
            // Already inside completion_tokens, so it is recorded and not
            // added to the bill a second time.
            event.reasoning_tokens =
                json_u64(v.pointer("/usage/completion_tokens_details/reasoning_tokens"));
            event.reasoning_seen = reasoning_seen;
            Some(event)
        }
        Wire::Anthropic => {
            let kind = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
            match kind {
                "content_block_delta" => {
                    let token = v
                        .pointer("/delta/text")
                        .and_then(|c| c.as_str())
                        .filter(|s| !s.is_empty())
                        .map(str::to_string);
                    let tool_args = v
                        .pointer("/delta/partial_json")
                        .and_then(|c| c.as_str())
                        .filter(|s| !s.is_empty())
                        .map(str::to_string);
                    let reasoning_seen = v.pointer("/delta/type").and_then(|c| c.as_str())
                        == Some("thinking_delta")
                        || v.pointer("/delta/thinking")
                            .and_then(|c| c.as_str())
                            .is_some_and(|s| !s.is_empty());
                    let mut event = InputUsage::empty().into_event(token, tool_args, None);
                    event.reasoning_seen = reasoning_seen;
                    Some(event)
                }
                "message_start" | "message_delta" => {
                    let input = anthropic_input(v);
                    let completion_tokens = json_u64(v.pointer("/usage/output_tokens"))
                        .or_else(|| json_u64(v.pointer("/message/usage/output_tokens")));
                    if input.prompt_tokens.is_none() && completion_tokens.is_none() {
                        return None;
                    }
                    Some(input.into_event(None, None, completion_tokens))
                }
                _ => None,
            }
        }
        Wire::Gemini | Wire::Vertex => {
            let mut text_out = String::new();
            let mut tool_args: Option<String> = None;
            let mut reasoning_seen = false;
            if let Some(parts) = v
                .pointer("/candidates/0/content/parts")
                .and_then(serde_json::Value::as_array)
            {
                for part in parts {
                    if part.get("thought").and_then(serde_json::Value::as_bool) == Some(true) {
                        reasoning_seen = true;
                        continue;
                    }
                    if let Some(t) = part.get("text").and_then(serde_json::Value::as_str) {
                        text_out.push_str(t);
                    }
                    if tool_args.is_none() {
                        if let Some(args) =
                            part.pointer("/functionCall/args").filter(|a| a.is_object())
                        {
                            tool_args = Some(args.to_string());
                        }
                    }
                }
            }
            let token = (!text_out.is_empty()).then_some(text_out);
            let usage = json_u64(v.pointer("/usageMetadata/candidatesTokenCount"));
            let input = gemini_input(v);
            // Thoughts are billed as output and are not inside candidatesTokenCount.
            let thoughts = json_u64(v.pointer("/usageMetadata/thoughtsTokenCount"));
            if token.is_none()
                && usage.is_none()
                && tool_args.is_none()
                && input.is_empty()
                && thoughts.is_none()
                && !reasoning_seen
            {
                return None;
            }
            let mut event = input.into_event(token, tool_args, usage);
            event.reasoning_tokens = thoughts;
            event.reasoning_billed_separately = thoughts.is_some();
            event.reasoning_seen = reasoning_seen;
            Some(event)
        }
        Wire::Cohere => {
            let kind = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
            let token = v
                .pointer("/delta/message/content/text")
                .and_then(|c| c.as_str())
                .or_else(|| {
                    if v.get("event_type").and_then(|e| e.as_str()) == Some("text-generation") {
                        v.get("text").and_then(|c| c.as_str())
                    } else {
                        None
                    }
                })
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            let tool_args = if kind == "tool-call-delta" {
                v.pointer("/delta/message/tool_calls/function/arguments")
                    .or_else(|| v.pointer("/delta/message/tool_calls/0/function/arguments"))
                    .and_then(|c| c.as_str())
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            } else {
                None
            };
            let usage = json_u64(v.pointer("/delta/usage/billed_units/output_tokens"))
                .or_else(|| json_u64(v.pointer("/response/meta/billed_units/output_tokens")))
                .or_else(|| json_u64(v.pointer("/usage/tokens/output_tokens")));
            let prompt_tokens = json_u64(v.pointer("/delta/usage/billed_units/input_tokens"))
                .or_else(|| json_u64(v.pointer("/response/meta/billed_units/input_tokens")))
                .or_else(|| json_u64(v.pointer("/usage/tokens/input_tokens")));
            let input = InputUsage {
                prompt_tokens,
                ..InputUsage::empty()
            };
            if token.is_none() && tool_args.is_none() && usage.is_none() && input.is_empty() {
                return None;
            }
            Some(input.into_event(token, tool_args, usage))
        }
        Wire::OpenAiResponses => {
            let kind = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
            match kind {
                "response.output_text.delta" => {
                    let token = v
                        .get("delta")
                        .and_then(|c| c.as_str())
                        .filter(|s| !s.is_empty())
                        .map(str::to_string);
                    token.map(|token| InputUsage::empty().into_event(Some(token), None, None))
                }
                "response.function_call_arguments.delta" => {
                    let args = v
                        .get("delta")
                        .and_then(|c| c.as_str())
                        .filter(|s| !s.is_empty())
                        .map(str::to_string);
                    args.map(|args| InputUsage::empty().into_event(None, Some(args), None))
                }
                "response.reasoning_summary_text.delta"
                | "response.reasoning_text.delta"
                | "response.reasoning.delta" => Some(StreamEvent {
                    reasoning_seen: true,
                    ..StreamEvent::default()
                }),
                "response.completed" | "response.incomplete" => {
                    let usage = &v["response"]["usage"];
                    let input = InputUsage {
                        prompt_tokens: json_u64(usage.get("input_tokens")),
                        cache_read_tokens: json_u64(
                            usage.pointer("/input_tokens_details/cached_tokens"),
                        ),
                        cache_write_tokens: None,
                        cache_write_1h_tokens: None,
                    };
                    let completion = json_u64(usage.get("output_tokens"));
                    if input.prompt_tokens.is_none() && completion.is_none() {
                        return None;
                    }
                    // Already inside output_tokens, so it is recorded and
                    // not added to the bill a second time.
                    let mut event = input.into_event(None, None, completion);
                    event.reasoning_tokens =
                        json_u64(usage.pointer("/output_tokens_details/reasoning_tokens"));
                    Some(event)
                }
                _ => None,
            }
        }
        Wire::Bedrock | Wire::Mock => None,
    }
}

/// Deterministic token stream used by the mock provider.
pub struct MockStream {
    n_tokens: usize,
    emitted: usize,
    delay: Duration,
    start: Option<tokio::time::Instant>,
    sleep: Option<std::panic::AssertUnwindSafe<Pin<Box<tokio::time::Sleep>>>>,
    usage_sent: bool,
}

impl MockStream {
    pub(super) fn new(delay: Duration, n_tokens: usize) -> Self {
        MockStream {
            n_tokens,
            emitted: 0,
            delay,
            start: None,
            sleep: None,
            usage_sent: false,
        }
    }
}

impl futures_util::Stream for MockStream {
    type Item = Result<StreamEvent>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        if self.emitted >= self.n_tokens {
            if self.usage_sent {
                return Poll::Ready(None);
            }
            self.usage_sent = true;
            return Poll::Ready(Some(Ok(InputUsage::empty().into_event(
                None,
                None,
                Some(self.n_tokens as u64),
            ))));
        }
        let start = *self.start.get_or_insert_with(tokio::time::Instant::now);
        let step = self
            .delay
            .saturating_mul(u32::try_from(self.emitted).unwrap_or(u32::MAX))
            .saturating_add(self.delay);
        let target = start + step;
        if let Some(sleep) = self.sleep.as_mut() {
            if sleep.0.deadline() != target {
                sleep.0.as_mut().reset(target);
            }
        } else {
            self.sleep = Some(std::panic::AssertUnwindSafe(Box::pin(
                tokio::time::sleep_until(target),
            )));
        }
        if let Some(sleep) = self.sleep.as_mut() {
            if std::future::Future::poll(sleep.0.as_mut(), cx).is_pending() {
                return Poll::Pending;
            }
        }
        self.emitted += 1;
        Poll::Ready(Some(Ok(InputUsage::empty().into_event(
            Some("token ".to_string()),
            None,
            None,
        ))))
    }
}

/// A live Bedrock `converse-stream` body decoded into
/// [`StreamEvent`]s via the binary event-stream frame parser.
pub struct BedrockStream {
    stream: Pin<Box<dyn futures_util::Stream<Item = reqwest::Result<bytes::Bytes>> + Send>>,
    buf: Vec<u8>,
    start: usize,
    prelude_checked: bool,
    pending: std::collections::VecDeque<Result<StreamEvent>>,
    upstream_done: bool,
    completed: bool,
    reasoning_emitted: bool,
}

impl BedrockStream {
    /// Maximum unconsumed bytes buffered while assembling a single Bedrock
    /// event-stream frame. Prevents a hostile stream from exhausting memory.
    const MAX_BUFFER_BYTES: usize = 4 * 1024 * 1024;

    pub(super) fn new(resp: reqwest::Response) -> Self {
        BedrockStream {
            stream: Box::pin(resp.bytes_stream()),
            buf: Vec::new(),
            start: 0,
            prelude_checked: false,
            pending: std::collections::VecDeque::new(),
            upstream_done: false,
            completed: false,
            reasoning_emitted: false,
        }
    }

    fn compact(&mut self) {
        if self.start == self.buf.len() {
            self.buf.clear();
            self.start = 0;
        } else if self.start > 64 * 1024 {
            self.buf.drain(..self.start);
            self.start = 0;
        }
    }

    fn enqueue_frame(&mut self, frame: &super::event_stream::EventFrame) -> Result<()> {
        use super::event_stream::EventFrame;
        let EventFrame {
            event_type,
            message_type,
            payload,
        } = frame;
        if message_type == "exception" || message_type == "error" {
            let body = String::from_utf8_lossy(payload);
            return Err(Error::Stream(format!(
                "Bedrock {message_type}: {}",
                crate::util::truncate(&body, 300)
            )));
        }
        let v: serde_json::Value = serde_json::from_slice(payload)
            .map_err(|e| Error::Protocol(format!("invalid Bedrock JSON: {e}")))?;
        match event_type.as_str() {
            // legacy invoke-with-response-stream shapes
            "chunk" | "contentBlockDelta" => {
                let token = v
                    .pointer("/delta/text")
                    .or_else(|| v.pointer("/bytes/text"))
                    .and_then(|c| c.as_str())
                    .filter(|s| !s.is_empty())
                    .map(str::to_string);
                let tool_args = v
                    .pointer("/delta/toolUse/input")
                    .and_then(|c| c.as_str())
                    .filter(|s| !s.is_empty())
                    .map(str::to_string);
                let reasoning_seen = event_type == "contentBlockDelta"
                    && v.pointer("/delta/reasoningContent")
                        .filter(|rc| !rc.is_null())
                        .is_some_and(|rc| {
                            rc.get("text")
                                .and_then(|t| t.as_str())
                                .is_some_and(|s| !s.is_empty())
                                || rc
                                    .get("redactedContent")
                                    .is_some_and(|r| !r.is_null() && r.as_str() != Some(""))
                        });
                if token.is_some() || tool_args.is_some() || reasoning_seen {
                    let mut event = InputUsage::empty().into_event(token, tool_args, None);
                    event.reasoning_seen = reasoning_seen;
                    if event.is_reasoning_only() {
                        if self.reasoning_emitted {
                            return Ok(());
                        }
                        self.reasoning_emitted = true;
                    }
                    self.pending.push_back(Ok(event));
                }
            }
            "messageStop" => self.completed = true,
            "metadata" => {
                if let Some(event) = bedrock_metadata_event(&v) {
                    self.pending.push_back(Ok(event));
                }
            }
            _ => {}
        }
        Ok(())
    }
}

impl futures_util::Stream for BedrockStream {
    type Item = Result<StreamEvent>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        loop {
            if let Some(event) = self.pending.pop_front() {
                return Poll::Ready(Some(event));
            }
            if self.upstream_done {
                return Poll::Ready(None);
            }
            match futures_util::StreamExt::poll_next_unpin(&mut self.stream, cx) {
                Poll::Ready(Some(Ok(chunk))) => {
                    let unconsumed = self.buf.len().saturating_sub(self.start);
                    if unconsumed.saturating_add(chunk.len()) > Self::MAX_BUFFER_BYTES {
                        self.upstream_done = true;
                        return Poll::Ready(Some(Err(Error::Protocol(format!(
                            "Bedrock event-stream buffer exceeded {} bytes",
                            Self::MAX_BUFFER_BYTES
                        )))));
                    }
                    self.buf.extend_from_slice(&chunk);
                    loop {
                        let window = &self.buf[self.start..];
                        if window.len() < 12 {
                            break;
                        }
                        let total = u32::from_be_bytes([window[0], window[1], window[2], window[3]])
                            as usize;
                        if !(16..=Self::MAX_BUFFER_BYTES).contains(&total) {
                            self.pending.push_back(Err(Error::Protocol(format!(
                                "event-stream: impossible total length {total}"
                            ))));
                            self.upstream_done = true;
                            break;
                        }
                        if window.len() < total {
                            if !self.prelude_checked {
                                if let Err(error) = super::event_stream::complete_frame_len(window)
                                {
                                    self.pending.push_back(Err(error));
                                    self.upstream_done = true;
                                    break;
                                }
                                self.prelude_checked = true;
                            }
                            break;
                        }
                        match super::event_stream::decode_frame(window) {
                            Ok((frame, used)) => {
                                self.start += used;
                                self.prelude_checked = false;
                                if let Err(error) = self.enqueue_frame(&frame) {
                                    self.pending.push_back(Err(error));
                                    self.upstream_done = true;
                                    break;
                                }
                            }
                            Err(error) => {
                                self.pending.push_back(Err(error));
                                self.upstream_done = true;
                                break;
                            }
                        }
                    }
                    self.compact();
                }
                Poll::Ready(Some(Err(e))) => {
                    self.upstream_done = true;
                    return Poll::Ready(Some(Err(Error::Stream(
                        super::request::format_reqwest_error(None, e),
                    ))));
                }
                Poll::Ready(None) => {
                    self.upstream_done = true;
                    if self.start < self.buf.len() || !self.completed {
                        self.pending.push_back(Err(Error::Protocol(
                            "Bedrock stream ended before a complete messageStop".into(),
                        )));
                    }
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bedrock_metadata_adds_cache_tokens_onto_uncached_input() {
        let event = bedrock_metadata_event(&serde_json::json!({
            "usage": {
                "inputTokens": 100,
                "outputTokens": 20,
                "cacheReadInputTokens": 400,
                "cacheWriteInputTokens": 25
            }
        }))
        .expect("metadata event");
        assert_eq!(event.prompt_tokens, Some(525));
        assert_eq!(event.cache_read_tokens, Some(400));
        assert_eq!(event.cache_write_tokens, Some(25));
        assert_eq!(event.cache_write_1h_tokens, None);
        assert_eq!(event.completion_tokens, Some(20));
    }

    #[test]
    fn cohere_v2_tool_call_delta_decodes_arguments() {
        let payload = r#"{"type":"tool-call-delta","index":0,"delta":{"message":{"tool_calls":{"function":{"arguments":"{\"city\":\"Paris\"}"}}}}}"#;
        let event = decode_payload(payload, Wire::Cohere).expect("cohere tool delta");
        assert_eq!(event.tool_args.as_deref(), Some(r#"{"city":"Paris"}"#));
        assert!(event.token.is_none());
    }

    #[test]
    fn gemini_and_vertex_skip_thought_parts_and_keep_all_visible_parts() {
        // Pure thought part must not emit visible token text (nor trigger TTFT),
        // while flagging reasoning_seen so budget exhaustion can be diagnosed.
        let thought_only =
            r#"{"candidates":[{"content":{"parts":[{"text":"internal chain","thought":true}]}}]}"#;
        for wire in [Wire::Gemini, Wire::Vertex] {
            let ev = decode_payload(thought_only, wire).expect("thought-only event");
            assert!(ev.token.is_none());
            assert!(ev.tool_args.is_none());
            assert!(ev.reasoning_seen);
        }

        // Thought part followed by multiple visible text parts concatenates visible parts only.
        let mixed = r#"{"candidates":[{"content":{"parts":[{"text":"secret","thought":true},{"text":"Hello"},{"text":", world"}]}}]}"#;
        for wire in [Wire::Gemini, Wire::Vertex] {
            let ev = decode_payload(mixed, wire).expect("mixed parts");
            assert_eq!(ev.token.as_deref(), Some("Hello, world"));
            assert!(ev.reasoning_seen);
        }

        // Thought part followed by functionCall still extracts tool_args.
        let thought_and_tool = r#"{"candidates":[{"content":{"parts":[{"text":"plan","thought":true},{"functionCall":{"name":"get_weather","args":{"city":"Paris"}}}]}}]}"#;
        let ev = decode_payload(thought_and_tool, Wire::Vertex).expect("tool call after thought");
        assert!(ev.token.is_none());
        assert_eq!(ev.tool_args.as_deref(), Some(r#"{"city":"Paris"}"#));
        assert!(ev.reasoning_seen);
    }

    #[test]
    fn deepseek_prompt_cache_hit_tokens_maps_to_cache_read_tokens() {
        let deepseek = r#"{"choices":[],"usage":{"prompt_tokens":1000,"completion_tokens":50,"prompt_cache_hit_tokens":800}}"#;
        let ev = decode_payload(deepseek, Wire::OpenAiCompat).expect("deepseek usage");
        assert_eq!(ev.prompt_tokens, Some(1000));
        assert_eq!(ev.cache_read_tokens, Some(800));
        assert_eq!(ev.completion_tokens, Some(50));

        // OpenAI prompt_tokens_details.cached_tokens takes precedence if both exist.
        let both = r#"{"choices":[],"usage":{"prompt_tokens":1000,"prompt_tokens_details":{"cached_tokens":600},"prompt_cache_hit_tokens":800}}"#;
        let ev = decode_payload(both, Wire::OpenAiCompat).expect("both");
        assert_eq!(ev.cache_read_tokens, Some(600));
    }

    #[test]
    fn reasoning_deltas_flag_reasoning_seen_without_starting_ttft_across_wires() {
        // OpenAI-compatible reasoning_content and reasoning fields
        for payload in [
            r#"{"choices":[{"delta":{"reasoning_content":"step 1"}}]}"#,
            r#"{"choices":[{"delta":{"reasoning":"step 1"}}]}"#,
        ] {
            let ev = decode_payload(payload, Wire::OpenAiCompat).expect("openai reasoning");
            assert!(ev.token.is_none());
            assert!(ev.tool_args.is_none());
            assert!(ev.reasoning_seen);
        }

        // OpenAI Responses reasoning event types
        for kind in [
            "response.reasoning_summary_text.delta",
            "response.reasoning_text.delta",
            "response.reasoning.delta",
        ] {
            let payload = format!(r#"{{"type":"{kind}","delta":"thinking"}}"#);
            let ev = decode_payload(&payload, Wire::OpenAiResponses).expect("responses reasoning");
            assert!(ev.token.is_none());
            assert!(ev.tool_args.is_none());
            assert!(ev.reasoning_seen);
        }

        // Anthropic thinking_delta and delta.thinking
        for payload in [
            r#"{"type":"content_block_delta","delta":{"type":"thinking_delta","thinking":"hmm"}}"#,
            r#"{"type":"content_block_delta","delta":{"thinking":"hmm"}}"#,
        ] {
            let ev = decode_payload(payload, Wire::Anthropic).expect("anthropic thinking");
            assert!(ev.token.is_none());
            assert!(ev.tool_args.is_none());
            assert!(ev.reasoning_seen);
        }
    }

    #[test]
    fn bedrock_metadata_parses_latency_ms_independently_and_validates_bounds() {
        // Usage + valid latencyMs
        let both = bedrock_metadata_event(&serde_json::json!({
            "usage": {"inputTokens": 50, "outputTokens": 10},
            "metrics": {"latencyMs": 1234.5}
        }))
        .expect("both");
        assert_eq!(both.prompt_tokens, Some(50));
        assert_eq!(both.completion_tokens, Some(10));
        assert_eq!(both.server_total_ms, Some(1234.5));

        // Metrics-only (missing usage)
        let metrics_only = bedrock_metadata_event(&serde_json::json!({
            "metrics": {"latencyMs": 420}
        }))
        .expect("metrics only");
        assert_eq!(metrics_only.prompt_tokens, None);
        assert_eq!(metrics_only.completion_tokens, None);
        assert_eq!(metrics_only.server_total_ms, Some(420.0));

        // Invalid latencyMs values are rejected (and return None when usage is absent)
        for bad in [
            serde_json::json!(-5.0),
            serde_json::json!(1e308),
            serde_json::json!(86_400_001.0),
            serde_json::json!("1234"),
            serde_json::json!(null),
        ] {
            assert!(
                bedrock_metadata_event(&serde_json::json!({"metrics": {"latencyMs": bad}}))
                    .is_none(),
                "expected None for bad latencyMs={bad}"
            );
            // When usage is present, bad latencyMs is ignored while usage is kept.
            let with_usage = bedrock_metadata_event(&serde_json::json!({
                "usage": {"outputTokens": 5},
                "metrics": {"latencyMs": bad}
            }))
            .expect("usage survives bad latencyMs");
            assert_eq!(with_usage.completion_tokens, Some(5));
            assert_eq!(with_usage.server_total_ms, None);
        }
    }

    #[test]
    fn sse_stream_coalesces_repeated_reasoning_only_deltas() {
        let mut stream = SseStream {
            stream: Box::pin(futures_util::stream::empty()),
            parser: crate::sse::SseParser::new(),
            wire: Wire::OpenAiCompat,
            pending: std::collections::VecDeque::new(),
            upstream_done: false,
            completed: false,
            reasoning_emitted: false,
            timing: ServerTiming::default(),
        };
        let block = r#"data: {"choices":[{"delta":{"reasoning_content":"step"}}]}"#;
        for _ in 0..50 {
            stream.enqueue_block(block);
        }
        assert_eq!(
            stream.pending.len(),
            1,
            "repeated reasoning-only SSE deltas must coalesce to a single event"
        );
        let first = stream
            .pending
            .pop_front()
            .expect("one event")
            .expect("ok event");
        assert!(first.reasoning_seen);
        assert!(first.token.is_none());

        // Visible token still passes through after reasoning coalescing.
        stream.enqueue_block(r#"data: {"choices":[{"delta":{"content":"hi"}}]}"#);
        let tok = stream
            .pending
            .pop_front()
            .expect("token event")
            .expect("ok token");
        assert_eq!(tok.token.as_deref(), Some("hi"));
    }

    #[test]
    fn bedrock_stream_detects_reasoning_content_and_coalesces_repeats() {
        use super::super::event_stream::EventFrame;
        let mut stream = BedrockStream {
            stream: Box::pin(futures_util::stream::empty()),
            buf: Vec::new(),
            start: 0,
            prelude_checked: false,
            pending: std::collections::VecDeque::new(),
            upstream_done: false,
            completed: false,
            reasoning_emitted: false,
        };
        let text_frame = EventFrame {
            event_type: "contentBlockDelta".into(),
            message_type: "event".into(),
            payload: br#"{"delta":{"reasoningContent":{"text":"thinking"}}}"#.to_vec(),
        };
        let redacted_frame = EventFrame {
            event_type: "contentBlockDelta".into(),
            message_type: "event".into(),
            payload: br#"{"delta":{"reasoningContent":{"redactedContent":"YWJj"}}}"#.to_vec(),
        };
        for _ in 0..10 {
            stream.enqueue_frame(&text_frame).expect("text reasoning");
            stream
                .enqueue_frame(&redacted_frame)
                .expect("redacted reasoning");
        }
        assert_eq!(
            stream.pending.len(),
            1,
            "repeated reasoning-only Bedrock frames must coalesce to one event"
        );
        let first = stream
            .pending
            .pop_front()
            .expect("one event")
            .expect("ok event");
        assert!(first.reasoning_seen);
        assert!(first.token.is_none());

        // Visible text delta still emits normally.
        let visible = EventFrame {
            event_type: "contentBlockDelta".into(),
            message_type: "event".into(),
            payload: br#"{"delta":{"text":"answer"}}"#.to_vec(),
        };
        stream.enqueue_frame(&visible).expect("visible delta");
        let tok = stream
            .pending
            .pop_front()
            .expect("visible event")
            .expect("ok visible");
        assert_eq!(tok.token.as_deref(), Some("answer"));
    }
}
