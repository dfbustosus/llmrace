//! Provider registry, wire protocols and streaming request plumbing.

use crate::error::{Error, Result};
use crate::sse::parse_data_events;
use serde_json::json;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

/// Streaming wire protocol spoken by a provider endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wire {
    /// OpenAI `/v1/chat/completions` streaming (also Groq, OpenRouter, Ollama, vLLM…).
    OpenAiCompat,
    /// Anthropic `/v1/messages` streaming.
    Anthropic,
    /// Google Gemini `streamGenerateContent` streaming.
    Gemini,
    /// Cohere `/v2/chat` streaming.
    Cohere,
    /// Built-in deterministic provider for tests and demos.
    Mock,
}

/// How the API key is presented to the endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AuthKind {
    /// `Authorization: Bearer <key>` (OpenAI-compatible default).
    #[default]
    Bearer,
    /// `api-key: <key>` (Azure OpenAI).
    ApiKeyHeader,
}

/// A fully-resolved provider description.
#[derive(Debug, Clone)]
pub struct ProviderSpec {
    /// Stable lowercase identifier used on the CLI (e.g. `openai`).
    pub id: Arc<str>,
    /// Name shown in the TUI and reports.
    pub display_name: Arc<str>,
    /// Absolute endpoint URL.
    pub endpoint: Arc<str>,
    /// Environment variable that holds the API key, if any.
    pub api_key_env: Option<Arc<str>>,
    /// Model used when `--model` is not given.
    pub default_model: Arc<str>,
    /// Wire protocol of the endpoint.
    pub wire: Wire,
    /// Authentication style for the API key.
    pub auth: AuthKind,
}

impl ProviderSpec {
    /// OpenAI official API.
    pub fn openai() -> Self {
        spec(
            "openai",
            "https://api.openai.com/v1/chat/completions",
            Some("OPENAI_API_KEY"),
            "gpt-4o-mini",
            Wire::OpenAiCompat,
        )
    }

    /// Anthropic official API.
    pub fn anthropic() -> Self {
        spec(
            "anthropic",
            "https://api.anthropic.com/v1/messages",
            Some("ANTHROPIC_API_KEY"),
            "claude-3-5-haiku-latest",
            Wire::Anthropic,
        )
    }

    /// Groq ultra-low-latency inference.
    pub fn groq() -> Self {
        spec(
            "groq",
            "https://api.groq.com/openai/v1/chat/completions",
            Some("GROQ_API_KEY"),
            "llama-3.3-70b-versatile",
            Wire::OpenAiCompat,
        )
    }

    /// OpenRouter multi-provider gateway.
    pub fn openrouter() -> Self {
        spec(
            "openrouter",
            "https://openrouter.ai/api/v1/chat/completions",
            Some("OPENROUTER_API_KEY"),
            "openai/gpt-4o-mini",
            Wire::OpenAiCompat,
        )
    }

    /// Google Gemini via `streamGenerateContent`.
    pub fn gemini() -> Self {
        spec(
            "gemini",
            "https://generativelanguage.googleapis.com/v1beta/models/{model}:streamGenerateContent?alt=sse",
            Some("GEMINI_API_KEY"),
            "gemini-2.0-flash",
            Wire::Gemini,
        )
    }

    /// xAI Grok official API.
    pub fn xai() -> Self {
        spec(
            "xai",
            "https://api.x.ai/v1/chat/completions",
            Some("XAI_API_KEY"),
            "grok-3-mini",
            Wire::OpenAiCompat,
        )
    }

    /// Perplexity Sonar API.
    pub fn perplexity() -> Self {
        spec(
            "perplexity",
            "https://api.perplexity.ai/chat/completions",
            Some("PERPLEXITY_API_KEY"),
            "sonar",
            Wire::OpenAiCompat,
        )
    }

    /// Cohere official API (v2 chat streaming).
    pub fn cohere() -> Self {
        spec(
            "cohere",
            "https://api.cohere.com/v2/chat",
            Some("CO_API_KEY"),
            "command-a-03-2025",
            Wire::Cohere,
        )
    }

    /// DeepSeek official API.
    pub fn deepseek() -> Self {
        spec(
            "deepseek",
            "https://api.deepseek.com/v1/chat/completions",
            Some("DEEPSEEK_API_KEY"),
            "deepseek-chat",
            Wire::OpenAiCompat,
        )
    }

    /// Mistral official API.
    pub fn mistral() -> Self {
        spec(
            "mistral",
            "https://api.mistral.ai/v1/chat/completions",
            Some("MISTRAL_API_KEY"),
            "mistral-small-latest",
            Wire::OpenAiCompat,
        )
    }

    /// Together inference cloud.
    pub fn together() -> Self {
        spec(
            "together",
            "https://api.together.xyz/v1/chat/completions",
            Some("TOGETHER_API_KEY"),
            "meta-llama/Llama-3.3-70B-Instruct-Turbo",
            Wire::OpenAiCompat,
        )
    }

    /// Fireworks inference cloud.
    pub fn fireworks() -> Self {
        spec(
            "fireworks",
            "https://api.fireworks.ai/inference/v1/chat/completions",
            Some("FIREWORKS_API_KEY"),
            "accounts/fireworks/models/llama-v3p3-70b-instruct",
            Wire::OpenAiCompat,
        )
    }

    /// Cerebras wafer-scale inference.
    pub fn cerebras() -> Self {
        spec(
            "cerebras",
            "https://api.cerebras.ai/v1/chat/completions",
            Some("CEREBRAS_API_KEY"),
            "llama-3.3-70b",
            Wire::OpenAiCompat,
        )
    }

    /// Local Ollama daemon.
    pub fn ollama() -> Self {
        spec(
            "ollama",
            "http://localhost:11434/v1/chat/completions",
            None,
            "llama3.2",
            Wire::OpenAiCompat,
        )
    }

    /// Deterministic in-process provider for tests and demos.
    pub fn mock() -> Self {
        spec("mock", "", None, "mock-1", Wire::Mock)
    }

    /// Builds a custom provider against any OpenAI-compatible endpoint
    /// (vLLM, LiteLLM proxy, Together, Fireworks…).
    pub fn custom(name: &str, endpoint: &str, key_env: Option<&str>, model: &str) -> Self {
        spec(name, endpoint, key_env, model, Wire::OpenAiCompat)
    }

    /// Builds an Azure OpenAI provider for a resource + deployment.
    ///
    /// Reads the key from `AZURE_OPENAI_API_KEY` and authenticates with the
    /// `api-key` header instead of a bearer token.
    pub fn azure_openai(resource: &str, deployment: &str) -> Self {
        let endpoint = format!(
            "https://{resource}.openai.azure.com/openai/deployments/{deployment}/chat/completions?api-version=2024-08-01-preview"
        );
        let mut p = spec(
            "azure",
            &endpoint,
            Some("AZURE_OPENAI_API_KEY"),
            deployment,
            Wire::OpenAiCompat,
        );
        p.auth = AuthKind::ApiKeyHeader;
        p
    }

    /// All built-in providers.
    pub fn registry() -> Vec<ProviderSpec> {
        vec![
            ProviderSpec::openai(),
            ProviderSpec::anthropic(),
            ProviderSpec::gemini(),
            ProviderSpec::groq(),
            ProviderSpec::xai(),
            ProviderSpec::perplexity(),
            ProviderSpec::cohere(),
            ProviderSpec::deepseek(),
            ProviderSpec::mistral(),
            ProviderSpec::together(),
            ProviderSpec::fireworks(),
            ProviderSpec::cerebras(),
            ProviderSpec::openrouter(),
            ProviderSpec::ollama(),
            ProviderSpec::mock(),
        ]
    }

    /// Looks up a built-in provider by CLI name.
    pub fn find(name: &str) -> Option<ProviderSpec> {
        ProviderSpec::registry()
            .into_iter()
            .find(|p| *p.id == *name)
    }

    /// `true` when the required API key is present in the environment.
    pub fn is_available(&self) -> bool {
        self.api_key_env
            .as_ref()
            .map(|env| std::env::var(env.as_ref()).is_ok())
            .unwrap_or(true)
    }

    fn api_key(&self) -> Result<Option<String>> {
        match &self.api_key_env {
            Some(env) => std::env::var(env.as_ref())
                .ok()
                .map(Some)
                .ok_or_else(|| Error::Config(format!("missing env var {}", env))),
            None => Ok(None),
        }
    }
}

fn spec(id: &str, endpoint: &str, key_env: Option<&str>, model: &str, wire: Wire) -> ProviderSpec {
    ProviderSpec {
        id: Arc::from(id),
        display_name: Arc::from(id),
        endpoint: Arc::from(endpoint),
        api_key_env: key_env.map(Arc::from),
        default_model: Arc::from(model),
        wire,
        auth: AuthKind::default(),
    }
}

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

/// Parameters of a single completion request.
#[derive(Debug, Clone)]
pub struct CompletionRequest<'a> {
    /// Model identifier sent to the provider.
    pub model: &'a str,
    /// User prompt.
    pub prompt: &'a str,
    /// Sampling cap passed as `max_tokens`.
    pub max_tokens: u64,
    /// Enable the tool-calling task and attach tool schemas.
    pub tools: bool,
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
}

impl EventStream {
    /// Returns provider-reported timing and identity metadata captured
    /// from the HTTP response headers, when available.
    pub fn server_timing(&self) -> ServerTiming {
        match self {
            EventStream::Http(s) => s.server_timing(),
            EventStream::Mock(_) => ServerTiming::default(),
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
        }
    }
}

/// Starts a streaming completion against `spec` and returns a live event stream.
///
/// ```
/// use llmrace::provider::{stream_completion, CompletionRequest, ProviderSpec};
///
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let client = reqwest::Client::new();
/// let spec = ProviderSpec::mock();
/// let req = CompletionRequest { model: "mock-1", prompt: "hi", max_tokens: 8, tools: false };
/// let mut stream = stream_completion(&client, &spec, &req).await.unwrap();
/// use futures_util::StreamExt;
/// let mut tokens = 0;
/// while let Some(ev) = stream.next().await {
///     if ev.unwrap().token.is_some() { tokens += 1; }
/// }
/// assert!(tokens > 0);
/// # });
/// ```
pub async fn stream_completion(
    client: &reqwest::Client,
    spec: &ProviderSpec,
    req: &CompletionRequest<'_>,
) -> Result<EventStream> {
    match spec.wire {
        Wire::Mock => Ok(EventStream::Mock(MockStream::new(
            Duration::from_millis(12),
            24,
        ))),
        Wire::OpenAiCompat => {
            let key = spec.api_key()?;
            let cap_key = if requires_completion_tokens(req.model) {
                "max_completion_tokens"
            } else {
                "max_tokens"
            };
            let mut body = json!({
                "model": req.model,
                "messages": [{"role": "user", "content": req.prompt}],
                "stream": true,
            });
            body[cap_key] = json!(req.max_tokens);
            if *spec.id != *"ollama" {
                body["stream_options"] = json!({"include_usage": true});
            }
            if req.tools {
                body["tools"] = crate::tools::weather_tool(Wire::OpenAiCompat);
            }
            let mut request = client.post(spec.endpoint.as_ref()).json(&body);
            if let Some(k) = &key {
                request = match spec.auth {
                    AuthKind::Bearer => request.bearer_auth(k),
                    AuthKind::ApiKeyHeader => request.header("api-key", k),
                };
            }
            let resp = request
                .send()
                .await
                .map_err(|e| Error::Stream(e.to_string()))?;
            let resp = check_status(spec, resp).await?;
            Ok(EventStream::Http(SseStream::new(resp, Wire::OpenAiCompat)))
        }
        Wire::Anthropic => {
            let key = spec.api_key()?;
            let mut body = json!({
                "model": req.model,
                "max_tokens": req.max_tokens,
                "messages": [{"role": "user", "content": req.prompt}],
                "stream": true,
            });
            if req.tools {
                body["tools"] = crate::tools::weather_tool(Wire::Anthropic);
            }
            let resp = client
                .post(spec.endpoint.as_ref())
                .header("x-api-key", key.as_deref().unwrap_or_default())
                .header("anthropic-version", "2023-06-01")
                .json(&body)
                .send()
                .await
                .map_err(|e| Error::Stream(e.to_string()))?;
            let resp = check_status(spec, resp).await?;
            Ok(EventStream::Http(SseStream::new(resp, Wire::Anthropic)))
        }
        Wire::Cohere => {
            let key = spec.api_key()?;
            let mut body = json!({
                "model": req.model,
                "messages": [{"role": "user", "content": req.prompt}],
                "max_tokens": req.max_tokens,
                "stream": true,
            });
            if req.tools {
                body["tools"] = crate::tools::weather_tool(Wire::Cohere);
            }
            let resp = client
                .post(spec.endpoint.as_ref())
                .bearer_auth(key.as_deref().unwrap_or_default())
                .json(&body)
                .send()
                .await
                .map_err(|e| Error::Stream(e.to_string()))?;
            let resp = check_status(spec, resp).await?;
            Ok(EventStream::Http(SseStream::new(resp, Wire::Cohere)))
        }
        Wire::Gemini => {
            let key = spec.api_key()?;
            let endpoint = spec.endpoint.replace("{model}", req.model);
            let mut body = json!({
                "contents": [{"parts": [{"text": req.prompt}]}],
                "generationConfig": {"maxOutputTokens": req.max_tokens},
            });
            if req.tools {
                body["tools"] = crate::tools::weather_tool(Wire::Gemini);
            }
            let resp = client
                .post(endpoint)
                .header("x-goog-api-key", key.as_deref().unwrap_or_default())
                .json(&body)
                .send()
                .await
                .map_err(|e| Error::Stream(e.to_string()))?;
            let resp = check_status(spec, resp).await?;
            Ok(EventStream::Http(SseStream::new(resp, Wire::Gemini)))
        }
    }
}

/// Splits a `--model` entry into a provider-specific override when written
/// as `provider=model`; plain entries (no `=`) are global and yield `None`.
pub fn split_model_override(entry: &str) -> Option<(String, String)> {
    let (provider, model) = entry.split_once('=')?;
    let provider = provider.trim();
    let model = model.trim();
    if provider.is_empty() || model.is_empty() {
        return None;
    }
    Some((provider.to_string(), model.to_string()))
}

/// OpenAI models that reject `max_tokens` and require
/// `max_completion_tokens` (gpt-5 family and the o-series reasoning models).
fn requires_completion_tokens(model: &str) -> bool {
    let m = model.rsplit('/').next().unwrap_or(model);
    m.starts_with("gpt-5") || m.starts_with("o1") || m.starts_with("o3") || m.starts_with("o4")
}

async fn check_status(spec: &ProviderSpec, resp: reqwest::Response) -> Result<reqwest::Response> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let body = resp.text().await.unwrap_or_default();
    let mut body = truncate(&body, 300);
    if *spec.id == *"ollama" && status.as_u16() == 404 && body.contains("not found") {
        body.push_str(
            " — the model is not pulled; run `ollama pull <model>` or pass `-m <model>` with an already-pulled tag",
        );
    }
    Err(Error::Http {
        provider: spec.id.to_string(),
        status: status.as_u16(),
        body,
    })
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
    fn new(resp: reqwest::Response, wire: Wire) -> Self {
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
        Wire::Mock => None,
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
    fn new(delay: Duration, n_tokens: usize) -> Self {
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

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}
