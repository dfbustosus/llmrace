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

/// The stream returned by [`stream_completion`].
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
    pending: std::collections::VecDeque<StreamEvent>,
    upstream_done: bool,
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
            .and_then(|v| v.trim().parse::<f64>().ok());
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
            timing,
        }
    }

    /// Returns captured response-header timing metadata.
    pub fn server_timing(&self) -> ServerTiming {
        self.timing.clone()
    }

    fn enqueue_block(&mut self, block: &str) {
        for payload in parse_data_events(block) {
            if let Some(event) = decode_payload(payload, self.wire) {
                let carries_output = event.token.is_some()
                    || event.completion_tokens.is_some()
                    || event.tool_args.is_some();
                if carries_output {
                    self.pending.push_back(event);
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
                return Poll::Ready(Some(Ok(event)));
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
                    Err(e) => return Poll::Ready(Some(Err(e))),
                },
                Poll::Ready(Some(Err(e))) => {
                    return Poll::Ready(Some(Err(Error::Stream(e.to_string()))));
                }
                Poll::Ready(None) => {
                    self.upstream_done = true;
                    for block in self.parser.finish() {
                        self.enqueue_block(&block);
                    }
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
            let usage = v
                .pointer("/usage/completion_tokens")
                .and_then(|c| c.as_u64());
            Some(StreamEvent {
                token,
                tool_args,
                completion_tokens: usage,
            })
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
                    Some(StreamEvent {
                        token,
                        tool_args,
                        completion_tokens: None,
                    })
                }
                "message_delta" => Some(StreamEvent {
                    tool_args: None,
                    token: None,
                    completion_tokens: v.pointer("/usage/output_tokens").and_then(|c| c.as_u64()),
                }),
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
            let usage = v
                .pointer("/usageMetadata/candidatesTokenCount")
                .and_then(|c| c.as_u64());
            if token.is_none() && usage.is_none() && tool_args.is_none() {
                return None;
            }
            Some(StreamEvent {
                token,
                tool_args,
                completion_tokens: usage,
            })
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
            let usage = v
                .pointer("/response/meta/billed_units/output_tokens")
                .and_then(|c| c.as_u64())
                .or_else(|| {
                    v.pointer("/usage/tokens/output_tokens")
                        .and_then(|c| c.as_u64())
                });
            if token.is_none() && usage.is_none() {
                return None;
            }
            Some(StreamEvent {
                tool_args: None,
                token,
                completion_tokens: usage,
            })
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
            return Poll::Ready(Some(Ok(StreamEvent {
                tool_args: None,
                token: None,
                completion_tokens: Some(self.n_tokens as u64),
            })));
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
        Poll::Ready(Some(Ok(StreamEvent {
            tool_args: None,
            token: Some("token ".to_string()),
            completion_tokens: None,
        })))
    }
}

/// A live Bedrock `invoke-with-response-stream` body decoded into
/// [`StreamEvent`]s via the binary event-stream frame parser.
pub struct BedrockStream {
    stream: Pin<Box<dyn futures_util::Stream<Item = reqwest::Result<bytes::Bytes>> + Send>>,
    buf: Vec<u8>,
    pending: std::collections::VecDeque<StreamEvent>,
    upstream_done: bool,
}

impl BedrockStream {
    pub(super) fn new(resp: reqwest::Response) -> Self {
        BedrockStream {
            stream: Box::pin(resp.bytes_stream()),
            buf: Vec::new(),
            pending: std::collections::VecDeque::new(),
            upstream_done: false,
        }
    }

    fn enqueue_frame(&mut self, frame: &super::event_stream::EventFrame) {
        use super::event_stream::EventFrame;
        let EventFrame {
            event_type,
            message_type,
            payload,
        } = frame;
        if message_type == "exception" {
            // surfaced via the stream ending / error on next poll
            let body = String::from_utf8_lossy(payload);
            let _ = crate::util::truncate(&body, 300);
            return;
        }
        let Ok(v): std::result::Result<serde_json::Value, _> = serde_json::from_slice(payload)
        else {
            return;
        };
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
                    self.pending.push_back(StreamEvent {
                        token,
                        tool_args,
                        completion_tokens: None,
                    });
                }
            }
            "messageStop" => {}
            "metadata" => {
                let usage = v
                    .pointer("/usage/outputTokens")
                    .or_else(|| v.pointer("/usage/output_tokens"))
                    .and_then(|c| c.as_u64());
                if let Some(n) = usage {
                    self.pending.push_back(StreamEvent {
                        token: None,
                        tool_args: None,
                        completion_tokens: Some(n),
                    });
                }
            }
            _ => {}
        }
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
                return Poll::Ready(Some(Ok(event)));
            }
            if self.upstream_done {
                return Poll::Ready(None);
            }
            match futures_util::StreamExt::poll_next_unpin(&mut self.stream, cx) {
                Poll::Ready(Some(Ok(chunk))) => {
                    self.buf.extend_from_slice(&chunk);
                    while let Ok((frame, used)) = super::event_stream::decode_frame(&self.buf) {
                        let frame = frame.clone();
                        self.buf.drain(..used);
                        self.enqueue_frame(&frame);
                    }
                }
                Poll::Ready(Some(Err(e))) => {
                    return Poll::Ready(Some(Err(Error::Stream(e.to_string()))))
                }
                Poll::Ready(None) => {
                    self.upstream_done = true;
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}
