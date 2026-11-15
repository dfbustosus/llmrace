//! Request building and dispatch: turns a [`CompletionRequest`] into a
//! wire-specific HTTP call and hands back the decoded event stream.

use super::spec::{AuthKind, ProviderSpec, Wire};
use super::stream::{EventStream, MockStream, SseStream};
use crate::error::{Error, Result};
use serde_json::json;
use std::time::Duration;

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
    /// Which tool task to attach (`crate::tools::registry` name).
    pub tool_task: Option<&'a str>,
}

/// How a built wire request authenticates.
#[derive(Debug, Clone)]
enum RequestAuth {
    /// No key required (local daemons, mock).
    None,
    /// `Authorization: Bearer <key>`.
    Bearer(String),
    /// Extra headers carrying the key (e.g. `x-api-key`, `x-goog-api-key`).
    Headers(Vec<(&'static str, String)>),
}

/// A wire-specific request, ready to send: URL, JSON body and auth.
struct WireRequest {
    url: String,
    body: serde_json::Value,
    auth: RequestAuth,
}

/// Builds the request for the given wire protocol; pure, no I/O.
fn build_wire_request(spec: &ProviderSpec, req: &CompletionRequest<'_>) -> Result<WireRequest> {
    let mut body = match spec.wire {
        Wire::Mock => json!(null),
        Wire::OpenAiCompat => {
            // gpt-5 / o-series reject `max_tokens` (HTTP 400) and require
            // `max_completion_tokens` instead.
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
            body
        }
        Wire::Anthropic | Wire::Cohere => json!({
            "model": req.model,
            "max_tokens": req.max_tokens,
            "messages": [{"role": "user", "content": req.prompt}],
            "stream": true,
        }),
        Wire::Gemini => json!({
            "contents": [{"parts": [{"text": req.prompt}]}],
            "generationConfig": {"maxOutputTokens": req.max_tokens},
        }),
    };
    if req.tools {
        let task = req
            .tool_task
            .and_then(crate::tools::find)
            .unwrap_or_else(default_tool_task);
        body["tools"] = crate::tools::tool_schema(&task, spec.wire);
    }

    let key = spec.api_key()?;
    let auth = match (spec.wire, spec.auth) {
        (Wire::Mock, _) => RequestAuth::None,
        (Wire::OpenAiCompat, AuthKind::ApiKeyHeader) => {
            RequestAuth::Headers(vec![("api-key", key.unwrap_or_default())])
        }
        (Wire::Anthropic, _) => RequestAuth::Headers(vec![
            ("x-api-key", key.clone().unwrap_or_default()),
            ("anthropic-version", "2023-06-01".to_string()),
        ]),
        (Wire::Gemini, _) => {
            RequestAuth::Headers(vec![("x-goog-api-key", key.clone().unwrap_or_default())])
        }
        _ => match key {
            Some(k) => RequestAuth::Bearer(k),
            None => RequestAuth::None,
        },
    };
    let url = if spec.wire == Wire::Gemini {
        spec.endpoint.replace("{model}", req.model)
    } else {
        spec.endpoint.to_string()
    };
    Ok(WireRequest { url, body, auth })
}

/// Sends a built wire request and returns the decoded SSE stream.
async fn send_wire_request(
    client: &reqwest::Client,
    spec: &ProviderSpec,
    wire_req: WireRequest,
) -> Result<EventStream> {
    let mut request = client.post(&wire_req.url).json(&wire_req.body);
    match &wire_req.auth {
        RequestAuth::None => {}
        RequestAuth::Bearer(key) => request = request.bearer_auth(key),
        RequestAuth::Headers(headers) => {
            for (name, value) in headers {
                request = request.header(*name, value);
            }
        }
    }
    let resp = request
        .send()
        .await
        .map_err(|e| Error::Stream(e.to_string()))?;
    let resp = check_status(spec, resp).await?;
    Ok(EventStream::Http(SseStream::new(resp, spec.wire)))
}

/// Starts a streaming completion against `spec` and returns a live event stream.
///
/// ```
/// use llmrace::provider::{stream_completion, CompletionRequest, ProviderSpec};
///
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let client = reqwest::Client::new();
/// let spec = ProviderSpec::mock();
/// let req = CompletionRequest { model: "mock-1", prompt: "hi", max_tokens: 8, tools: false, tool_task: None };
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
    if spec.wire == Wire::Mock {
        return Ok(EventStream::Mock(MockStream::new(
            Duration::from_millis(12),
            24,
        )));
    }
    send_wire_request(client, spec, build_wire_request(spec, req)?).await
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

fn default_tool_task() -> crate::tools::ToolTask {
    crate::tools::find("weather").unwrap_or_else(|| {
        // unreachable while the registry contains "weather"; avoids
        // panicking in builds where the registry is trimmed
        crate::tools::registry()
            .into_iter()
            .next()
            .unwrap_or(crate::tools::ToolTask {
                name: "weather",
                description: "",
                function: "get_weather",
                prompt: "",
                parameters: || serde_json::json!(null),
                validate: |_| false,
            })
    })
}

/// `true` for reasoning models (gpt-5 family, o-series): they think before
/// the first visible token and reject `max_tokens` in favor of
/// `max_completion_tokens`.
#[must_use]
pub fn is_reasoning_model(model: &str) -> bool {
    requires_completion_tokens(model)
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
    let mut body = crate::util::truncate(&body, 300);
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
