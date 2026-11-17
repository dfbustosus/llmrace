//! Request building and dispatch: turns a [`CompletionRequest`] into a
//! wire-specific HTTP call and hands back the decoded event stream.

use super::spec::{AuthKind, ProviderSpec, Wire};
use super::stream::{BedrockStream, EventStream, MockStream, SseStream};
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
        Wire::Mock | Wire::Bedrock | Wire::Vertex => json!(null),
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

    if spec.wire == Wire::Bedrock {
        let url = spec.endpoint.replace("{model}", req.model);
        return Ok(WireRequest {
            url,
            body,
            auth: RequestAuth::None,
        });
    }
    if spec.wire == Wire::Vertex {
        let url = spec.endpoint.replace("{model}", req.model);
        // token fetched at send time (async); auth is None here
        return Ok(WireRequest {
            url,
            body,
            auth: RequestAuth::None,
        });
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
    if wire_req.url.starts_with("https://bedrock-runtime.") {
        return send_bedrock(client, spec, wire_req).await;
    }
    if spec.wire == Wire::Vertex {
        return send_vertex(client, spec, wire_req).await;
    }
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

/// Lists the models a provider currently offers, via its models endpoint.
///
/// OpenAI-compatible providers answer `GET …/models` with
/// `{"data":[{"id":…}]}`; Anthropic uses the same shape, Gemini returns
/// `{"models":[{"name":"models/<id>"}]}` and Cohere an array of
/// `{"name":…}`. The mock wire has no models endpoint.
pub async fn list_models(client: &reqwest::Client, spec: &ProviderSpec) -> Result<Vec<String>> {
    use serde_json::Value;
    if spec.wire == Wire::Mock {
        return Err(Error::Config(format!(
            "{}: mock provider has no model listing",
            spec.id
        )));
    }
    let key = spec.api_key()?;
    // URL derivation via Url::join — proper RFC 3986 semantics instead of
    // string surgery, so custom endpoints with query strings, ports or
    // trailing paths resolve correctly.
    let models_url = |suffix: &str, replace_last: bool| -> Result<String> {
        let base = reqwest::Url::parse(&spec.endpoint)
            .map_err(|e| Error::Config(format!("invalid endpoint for {}: {e}", spec.id)))?;
        let mut url = base.clone();
        if replace_last {
            // ".../chat/completions" -> ".../chat/models"
            let mut segments = base.path_segments().map(Iterator::collect::<Vec<_>>);
            if let Some(segs) = segments.as_mut() {
                if segs.last() == Some(&"completions") || segs.last() == Some(&"messages") {
                    segs.pop();
                }
                url.set_path(&segs.join("/"));
            }
        }
        url.join(suffix)
            .map(|u| u.to_string())
            .map_err(|e| Error::Config(format!("cannot join models path for {}: {e}", spec.id)))
    };
    let (url, auth) = match spec.wire {
        Wire::OpenAiCompat => (models_url("models", true)?, bearer_or_none(key)),
        Wire::Anthropic => (
            models_url("models", true)?,
            RequestAuth::Headers(vec![
                ("x-api-key", key.clone().unwrap_or_default()),
                ("anthropic-version", "2023-06-01".to_string()),
            ]),
        ),
        Wire::Gemini => (
            spec.endpoint
                .split("/models")
                .next()
                .unwrap_or(spec.endpoint.as_ref())
                .trim_end_matches('/')
                .to_string()
                + "/models",
            RequestAuth::Headers(vec![("x-goog-api-key", key.clone().unwrap_or_default())]),
        ),
        Wire::Cohere => (
            spec.endpoint.trim_end_matches("/chat").to_string() + "/models",
            bearer_or_none(key),
        ),
        Wire::Vertex => {
            // vertex models list reuses the Gemini path with an OAuth token
            let region =
                std::env::var("GOOGLE_CLOUD_REGION").unwrap_or_else(|_| "us-central1".to_string());
            (
                format!("https://{region}-aiplatform.googleapis.com/v1/publishers/google/models"),
                RequestAuth::None, // OAuth at request time
            )
        }
        // Bedrock lists models via the ListFoundationModels control-plane
        // API, signed like every other Bedrock call.
        Wire::Bedrock => {
            let region = std::env::var("AWS_REGION").unwrap_or_else(|_| "us-east-1".to_string());
            (
                format!("https://bedrock.{region}.amazonaws.com/foundation-models"),
                RequestAuth::None, // signed below
            )
        }
        Wire::Mock => unreachable!("handled above"),
    };
    if spec.wire == Wire::Bedrock {
        return list_bedrock_models(client, spec, &url).await;
    }
    let mut request = client.get(&url);
    match &auth {
        RequestAuth::None => {}
        RequestAuth::Bearer(k) => request = request.bearer_auth(k),
        RequestAuth::Headers(headers) => {
            for (name, value) in headers {
                request = request.header(*name, value);
            }
        }
    }
    let resp = request
        .send()
        .await
        .map_err(|e| Error::Stream(format!("{}: {e}", spec.id)))?;
    let resp = check_status(spec, resp).await?;
    let body: Value = resp
        .json()
        .await
        .map_err(|e| Error::Stream(format!("{}: invalid models response: {e}", spec.id)))?;
    let mut ids: Vec<String> = Vec::new();
    if let Some(data) = body.get("data").and_then(Value::as_array) {
        for m in data {
            if let Some(id) = m.get("id").and_then(Value::as_str) {
                ids.push(id.to_string());
            }
        }
    } else if let Some(models) = body.get("models").and_then(Value::as_array) {
        for m in models {
            if let Some(name) = m.get("name").and_then(Value::as_str) {
                // Gemini names look like "models/gemini-2.0-flash"
                ids.push(name.rsplit('/').next().unwrap_or(name).to_string());
            }
        }
    } else if let Some(arr) = body.as_array() {
        for m in arr {
            if let Some(name) = m.get("name").and_then(Value::as_str) {
                ids.push(name.to_string());
            }
        }
    }
    ids.sort();
    ids.dedup();
    Ok(ids)
}

fn bearer_or_none(key: Option<String>) -> RequestAuth {
    match key {
        Some(k) => RequestAuth::Bearer(k),
        None => RequestAuth::None,
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
    // OpenAI-style model errors: point at live model discovery
    if body.contains("model_not_found")
        || (status.as_u16() == 404 && body.contains("does not exist"))
        || (status.as_u16() == 403 && body.contains("does not have access to model"))
    {
        body.push_str(&format!(
            " — list what this provider offers with `llmrace --models {}`",
            spec.id
        ));
    }
    Err(Error::Http {
        provider: spec.id.to_string(),
        status: status.as_u16(),
        body,
    })
}

/// Sends a SigV4-signed Bedrock request and returns the event-stream.
async fn send_bedrock(
    client: &reqwest::Client,
    spec: &ProviderSpec,
    wire_req: WireRequest,
) -> Result<EventStream> {
    use super::sigv4;

    let creds = sigv4::credentials_from_env().ok_or_else(sigv4::missing_credentials_error)?;
    let region = std::env::var("AWS_REGION").unwrap_or_else(|_| "us-east-1".to_string());
    let url = reqwest::Url::parse(&wire_req.url)
        .map_err(|e| Error::Config(format!("invalid bedrock endpoint: {e}")))?;
    let host = url.host_str().unwrap_or_default().to_string();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let body = serde_json::to_vec(&wire_req.body)
        .map_err(|e| Error::Config(format!("bedrock body encode: {e}")))?;
    let signed = sigv4::sign(
        &creds,
        &region,
        &host,
        "POST",
        url.path(),
        "",
        &body,
        &sigv4::amz_date(now),
    );

    let mut request = client
        .post(url)
        .header("x-amz-date", &signed.amz_date)
        .header("x-amz-content-sha256", &signed.content_sha256)
        .header("authorization", &signed.authorization)
        .header("content-type", "application/json")
        .body(body);
    if let Some(token) = &signed.session_token {
        request = request.header("x-amz-security-token", token);
    }
    let resp = request
        .send()
        .await
        .map_err(|e| Error::Stream(format!("{}: {e}", spec.id)))?;
    let resp = check_status(spec, resp).await?;
    Ok(EventStream::Bedrock(BedrockStream::new(resp)))
}

/// GET ListFoundationModels with SigV4, returning model ids.
async fn list_bedrock_models(
    client: &reqwest::Client,
    spec: &ProviderSpec,
    url: &str,
) -> Result<Vec<String>> {
    use super::sigv4;
    use serde_json::Value;

    let creds = sigv4::credentials_from_env().ok_or_else(sigv4::missing_credentials_error)?;
    let region = std::env::var("AWS_REGION").unwrap_or_else(|_| "us-east-1".to_string());
    let parsed = reqwest::Url::parse(url)
        .map_err(|e| Error::Config(format!("invalid bedrock models url: {e}")))?;
    let host = parsed.host_str().unwrap_or_default().to_string();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let signed = sigv4::sign(
        &creds,
        &region,
        &host,
        "GET",
        parsed.path(),
        parsed.query().unwrap_or(""),
        b"",
        &sigv4::amz_date(now),
    );
    let mut request = client
        .get(parsed)
        .header("x-amz-date", &signed.amz_date)
        .header("x-amz-content-sha256", &signed.content_sha256)
        .header("authorization", &signed.authorization)
        .header("accept", "application/json");
    if let Some(token) = &signed.session_token {
        request = request.header("x-amz-security-token", token);
    }
    let resp = request
        .send()
        .await
        .map_err(|e| Error::Stream(format!("{}: {e}", spec.id)))?;
    let resp = check_status(spec, resp).await?;
    let body: Value = resp
        .json()
        .await
        .map_err(|e| Error::Stream(format!("{}: invalid models response: {e}", spec.id)))?;
    let mut ids: Vec<String> = Vec::new();
    if let Some(models) = body.get("modelSummaries").and_then(Value::as_array) {
        for m in models {
            if let Some(id) = m.get("modelId").and_then(Value::as_str) {
                ids.push(id.to_string());
            }
        }
    }
    ids.sort();
    ids.dedup();
    Ok(ids)
}

/// Fetches an OAuth2 token and sends the Vertex request (Gemini SSE wire).
async fn send_vertex(
    client: &reqwest::Client,
    spec: &ProviderSpec,
    wire_req: WireRequest,
) -> Result<EventStream> {
    let account = super::gcp_oauth::load_service_account()?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let cached = super::gcp_oauth::fetch_token(&account, now).await?;
    let resp = client
        .post(&wire_req.url)
        .bearer_auth(&cached.token)
        .json(&wire_req.body)
        .send()
        .await
        .map_err(|e| Error::Stream(format!("{}: {e}", spec.id)))?;
    let resp = check_status(spec, resp).await?;
    Ok(EventStream::Http(SseStream::new(resp, Wire::Gemini)))
}
