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
fn openai_input(v: &serde_json::Value) -> InputUsage {
    InputUsage {
        prompt_tokens: json_u64(v.pointer("/usage/prompt_tokens")),
        cache_read_tokens: json_u64(v.pointer("/usage/prompt_tokens_details/cached_tokens")),
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
    let usage = v.get("usage")?;
    let input = bedrock_input(usage);
    let completion = field_u64(usage, &["outputTokens", "output_tokens"]);
    if completion.is_none() && input.is_empty() {
        return None;
    }
    Some(input.into_event(None, None, completion))
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
            timing,
        }
    }

    /// Returns captured response-header timing metadata.
    pub fn server_timing(&self) -> ServerTiming {
        self.timing.clone()
    }

    fn enqueue_block(&mut self, block: &str) {
        let payload = parse_data_events(block).join("\n");
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
            Wire::Gemini => value
                .pointer("/candidates/0/finishReason")
                .is_some_and(|v| v.is_string()),
            Wire::Cohere => {
                kind == "message-end"
                    || value.get("event_type").and_then(|v| v.as_str()) == Some("stream-end")
            }
            Wire::OpenAiResponses => matches!(kind, "response.completed" | "response.incomplete"),
            _ => false,
        };
        {
            let payload = payload.as_str();
            if let Some(event) = decode_payload(payload, self.wire) {
                let carries_output = event.token.is_some()
                    || event.completion_tokens.is_some()
                    || event.tool_args.is_some()
                    || event.prompt_tokens.is_some()
                    || event.cache_read_tokens.is_some()
                    || event.cache_write_tokens.is_some()
                    || event.cache_write_1h_tokens.is_some()
                    || event.reasoning_tokens.is_some();
                if carries_output {
                    self.pending.push_back(Ok(event));
                }
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
                    return Poll::Ready(Some(Err(Error::Stream(e.to_string()))));
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
            let usage = json_u64(v.pointer("/usage/completion_tokens"));
            let mut event = openai_input(&v).into_event(token, tool_args, usage);
            // Already inside completion_tokens, so it is recorded and not
            // added to the bill a second time.
            event.reasoning_tokens =
                json_u64(v.pointer("/usage/completion_tokens_details/reasoning_tokens"));
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
                    Some(InputUsage::empty().into_event(token, tool_args, None))
                }
                "message_start" | "message_delta" => {
                    let input = anthropic_input(&v);
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
        Wire::Gemini => {
            let token = v
                .pointer("/candidates/0/content/parts/0/text")
                .and_then(|c| c.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            let tool_args = v
                .pointer("/candidates/0/content/parts/0/functionCall/args")
                .filter(|a| a.is_object())
                .map(|a| a.to_string());
            let usage = json_u64(v.pointer("/usageMetadata/candidatesTokenCount"));
            let input = gemini_input(&v);
            // Thoughts are billed as output and are not inside candidatesTokenCount.
            let thoughts = json_u64(v.pointer("/usageMetadata/thoughtsTokenCount"));
            if token.is_none()
                && usage.is_none()
                && tool_args.is_none()
                && input.is_empty()
                && thoughts.is_none()
            {
                return None;
            }
            let mut event = input.into_event(token, tool_args, usage);
            event.reasoning_tokens = thoughts;
            event.reasoning_billed_separately = thoughts.is_some();
            Some(event)
        }
        Wire::Cohere => {
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
            if token.is_none() && usage.is_none() && input.is_empty() {
                return None;
            }
            Some(input.into_event(token, None, usage))
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
        Wire::Bedrock | Wire::Vertex | Wire::Mock => None,
    }
}

/// Deterministic token stream used by the mock provider.
pub struct MockStream {
    n_tokens: usize,
    emitted: usize,
    delay: Duration,
    start: Option<tokio::time::Instant>,
    usage_sent: bool,
}

impl MockStream {
    pub(super) fn new(delay: Duration, n_tokens: usize) -> Self {
        MockStream {
            n_tokens,
            emitted: 0,
            delay,
            start: None,
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
        let target = start + self.delay * self.emitted as u32 + self.delay;
        let now = tokio::time::Instant::now();
        if now < target {
            let waker = cx.waker().clone();
            tokio::spawn(async move {
                tokio::time::sleep_until(target).await;
                waker.wake();
            });
            return Poll::Pending;
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
    pending: std::collections::VecDeque<Result<StreamEvent>>,
    upstream_done: bool,
    completed: bool,
}

impl BedrockStream {
    pub(super) fn new(resp: reqwest::Response) -> Self {
        BedrockStream {
            stream: Box::pin(resp.bytes_stream()),
            buf: Vec::new(),
            pending: std::collections::VecDeque::new(),
            upstream_done: false,
            completed: false,
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
                if token.is_some() || tool_args.is_some() {
                    self.pending
                        .push_back(Ok(InputUsage::empty().into_event(token, tool_args, None)));
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
                    self.buf.extend_from_slice(&chunk);
                    loop {
                        let decoded =
                            super::event_stream::complete_frame_len(&self.buf).and_then(|ready| {
                                ready
                                    .map(|_| super::event_stream::decode_frame(&self.buf))
                                    .transpose()
                            });
                        match decoded {
                            Ok(Some((frame, used))) => {
                                self.buf.drain(..used);
                                if let Err(error) = self.enqueue_frame(&frame) {
                                    self.pending.push_back(Err(error));
                                    self.upstream_done = true;
                                    break;
                                }
                            }
                            Ok(None) => break,
                            Err(error) => {
                                self.pending.push_back(Err(error));
                                self.upstream_done = true;
                                break;
                            }
                        }
                    }
                }
                Poll::Ready(Some(Err(e))) => {
                    self.upstream_done = true;
                    return Poll::Ready(Some(Err(Error::Stream(e.to_string()))));
                }
                Poll::Ready(None) => {
                    self.upstream_done = true;
                    if !self.buf.is_empty() || !self.completed {
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
}
