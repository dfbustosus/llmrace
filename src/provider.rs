use anyhow::{anyhow, Result};
use serde_json::json;
use std::collections::VecDeque;
use std::pin::Pin;
use std::task::Poll;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wire {
    OpenAiCompat,
    Anthropic,
    Mock,
}

#[derive(Debug, Clone)]
pub struct Provider {
    pub name: &'static str,
    pub base_url: String,
    pub api_key_env: Option<&'static str>,
    pub default_model: &'static str,
    pub wire: Wire,
}

impl Provider {
    pub fn openai() -> Self {
        Provider {
            name: "openai",
            base_url: "https://api.openai.com/v1/chat/completions".into(),
            api_key_env: Some("OPENAI_API_KEY"),
            default_model: "gpt-4o-mini",
            wire: Wire::OpenAiCompat,
        }
    }

    pub fn groq() -> Self {
        Provider {
            name: "groq",
            base_url: "https://api.groq.com/openai/v1/chat/completions".into(),
            api_key_env: Some("GROQ_API_KEY"),
            default_model: "llama-3.3-70b-versatile",
            wire: Wire::OpenAiCompat,
        }
    }

    pub fn openrouter() -> Self {
        Provider {
            name: "openrouter",
            base_url: "https://openrouter.ai/api/v1/chat/completions".into(),
            api_key_env: Some("OPENROUTER_API_KEY"),
            default_model: "openai/gpt-4o-mini",
            wire: Wire::OpenAiCompat,
        }
    }

    pub fn ollama() -> Self {
        Provider {
            name: "ollama",
            base_url: "http://localhost:11434/v1/chat/completions".into(),
            api_key_env: None,
            default_model: "llama3.2",
            wire: Wire::OpenAiCompat,
        }
    }

    pub fn anthropic() -> Self {
        Provider {
            name: "anthropic",
            base_url: "https://api.anthropic.com/v1/messages".into(),
            api_key_env: Some("ANTHROPIC_API_KEY"),
            default_model: "claude-3-5-haiku-latest",
            wire: Wire::Anthropic,
        }
    }

    pub fn mock() -> Self {
        Provider {
            name: "mock",
            base_url: String::new(),
            api_key_env: None,
            default_model: "mock-1",
            wire: Wire::Mock,
        }
    }

    pub fn registry() -> Vec<Provider> {
        vec![
            Provider::openai(),
            Provider::anthropic(),
            Provider::groq(),
            Provider::openrouter(),
            Provider::ollama(),
            Provider::mock(),
        ]
    }

    pub fn find(name: &str) -> Option<Provider> {
        Provider::registry().into_iter().find(|p| p.name == name)
    }

    fn key(&self) -> Result<Option<String>> {
        match self.api_key_env {
            Some(env) => std::env::var(env)
                .ok()
                .map(Some)
                .ok_or_else(|| anyhow!("missing env var {}", env)),
            None => Ok(None),
        }
    }

    pub fn is_available(&self) -> bool {
        self.api_key_env
            .map(|env| std::env::var(env).is_ok())
            .unwrap_or(true)
    }
}

#[derive(Debug, Clone, Default)]
pub struct StreamEvent {
    pub token: Option<String>,
    pub completion_tokens: Option<u64>,
}

pub struct CompletionRequest<'a> {
    pub model: &'a str,
    pub prompt: &'a str,
    pub max_tokens: u64,
}

pub type EventStream = Pin<Box<dyn futures_util::Stream<Item = Result<StreamEvent>> + Send>>;

pub async fn stream_completion(
    client: &reqwest::Client,
    provider: &Provider,
    req: &CompletionRequest<'_>,
) -> Result<EventStream> {
    match provider.wire {
        Wire::Mock => Ok(Box::pin(MockStream::new(Duration::from_millis(12), 24))),
        Wire::OpenAiCompat => {
            let key = provider.key()?;
            let mut body = json!({
                "model": req.model,
                "messages": [{"role": "user", "content": req.prompt}],
                "max_tokens": req.max_tokens,
                "stream": true,
            });
            if provider.name != "ollama" {
                body["stream_options"] = json!({"include_usage": true});
            }
            let mut r = client.post(&provider.base_url).json(&body);
            if let Some(k) = &key {
                r = r.bearer_auth(k);
            }
            let resp = r.send().await?;
            check_status(provider, resp)
                .await
                .map(|resp| Box::pin(SseStream::new(resp, Wire::OpenAiCompat)) as EventStream)
        }
        Wire::Anthropic => {
            let key = provider.key()?;
            let body = json!({
                "model": req.model,
                "max_tokens": req.max_tokens,
                "messages": [{"role": "user", "content": req.prompt}],
                "stream": true,
            });
            let resp = client
                .post(&provider.base_url)
                .header("x-api-key", key.as_deref().unwrap_or_default())
                .header("anthropic-version", "2023-06-01")
                .json(&body)
                .send()
                .await?;
            check_status(provider, resp)
                .await
                .map(|resp| Box::pin(SseStream::new(resp, Wire::Anthropic)) as EventStream)
        }
    }
}

async fn check_status(provider: &Provider, resp: reqwest::Response) -> Result<reqwest::Response> {
    let status = resp.status();
    if status.is_success() {
        Ok(resp)
    } else {
        let text = resp.text().await.unwrap_or_default();
        Err(anyhow!(
            "{} HTTP {}: {}",
            provider.name,
            status,
            truncate(&text, 300)
        ))
    }
}

pub enum RawEvent {
    Delta(String),
    Usage(u64),
}

pub fn parse_sse_chunk(chunk: &str, wire: Wire) -> Vec<RawEvent> {
    let mut out = Vec::new();
    for line in chunk.split('\n') {
        let line = line.trim();
        let Some(payload) = line.strip_prefix("data:") else {
            continue;
        };
        let payload = payload.trim();
        if payload == "[DONE]" {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(payload) else {
            continue;
        };
        match wire {
            Wire::OpenAiCompat => {
                if let Some(delta) = v
                    .pointer("/choices/0/delta/content")
                    .and_then(|c| c.as_str())
                {
                    if !delta.is_empty() {
                        out.push(RawEvent::Delta(delta.to_string()));
                    }
                }
                if let Some(n) = v
                    .pointer("/usage/completion_tokens")
                    .and_then(|c| c.as_u64())
                {
                    out.push(RawEvent::Usage(n));
                }
            }
            Wire::Anthropic => {
                let kind = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
                if kind == "content_block_delta" {
                    if let Some(delta) = v.pointer("/delta/text").and_then(|c| c.as_str()) {
                        if !delta.is_empty() {
                            out.push(RawEvent::Delta(delta.to_string()));
                        }
                    }
                }
                if kind == "message_delta" {
                    if let Some(n) = v.pointer("/usage/output_tokens").and_then(|c| c.as_u64()) {
                        out.push(RawEvent::Usage(n));
                    }
                }
            }
            Wire::Mock => {}
        }
    }
    out
}

pub struct SseStream {
    stream: Pin<Box<dyn futures_util::Stream<Item = reqwest::Result<bytes::Bytes>> + Send>>,
    buf: Vec<u8>,
    wire: Wire,
    pending: VecDeque<Result<StreamEvent>>,
    upstream_done: bool,
}

impl SseStream {
    fn new(resp: reqwest::Response, wire: Wire) -> Self {
        SseStream {
            stream: Box::pin(resp.bytes_stream()),
            buf: Vec::new(),
            wire,
            pending: VecDeque::new(),
            upstream_done: false,
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
                Poll::Ready(Some(Ok(bytes))) => {
                    self.buf.extend_from_slice(&bytes);
                    self.drain_complete_chunks();
                }
                Poll::Ready(Some(Err(e))) => {
                    return Poll::Ready(Some(Err(anyhow!("stream error: {}", e))));
                }
                Poll::Ready(None) => {
                    self.upstream_done = true;
                    self.flush_remaining();
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

impl SseStream {
    fn drain_complete_chunks(&mut self) {
        while let Some(pos) = find_separator(&self.buf) {
            let chunk: Vec<u8> = self.buf.drain(..pos).collect();
            self.buf.drain(..separator_len(&self.buf));
            self.enqueue_chunk(&chunk);
        }
    }

    fn flush_remaining(&mut self) {
        if !self.buf.is_empty() {
            let chunk = std::mem::take(&mut self.buf);
            self.enqueue_chunk(&chunk);
        }
    }

    fn enqueue_chunk(&mut self, chunk: &[u8]) {
        let text = String::from_utf8_lossy(chunk).to_string();
        let mut usage: Option<u64> = None;
        for event in parse_sse_chunk(&text, self.wire) {
            match event {
                RawEvent::Delta(d) => self.pending.push_back(Ok(StreamEvent {
                    token: Some(d),
                    completion_tokens: None,
                })),
                RawEvent::Usage(n) => usage = Some(n),
            }
        }
        if let Some(n) = usage {
            self.pending.push_back(Ok(StreamEvent {
                token: None,
                completion_tokens: Some(n),
            }));
        }
    }
}

struct MockStream {
    n_tokens: usize,
    emitted: usize,
    delay: Duration,
    elapsed: Option<tokio::time::Instant>,
    usage_sent: bool,
}

impl MockStream {
    fn new(delay: Duration, n_tokens: usize) -> Self {
        MockStream {
            n_tokens,
            emitted: 0,
            delay,
            elapsed: None,
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
                token: None,
                completion_tokens: Some(self.n_tokens as u64),
            })));
        }
        let start = *self.elapsed.get_or_insert_with(tokio::time::Instant::now);
        let target = start + self.delay * (self.emitted as u32 + 1);
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
            token: Some("token ".to_string()),
            completion_tokens: None,
        })))
    }
}

fn find_separator(buf: &[u8]) -> Option<usize> {
    buf.windows(2)
        .position(|w| w == b"\n\n")
        .or_else(|| buf.windows(4).position(|w| w == b"\r\n\r\n"))
}

fn separator_len(buf: &[u8]) -> usize {
    if buf.starts_with(b"\r\n\r\n") {
        4
    } else {
        2
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}
