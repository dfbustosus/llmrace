//! Request building and dispatch: turns a [`CompletionRequest`] into a
//! wire-specific HTTP call and hands back the decoded event stream.

use super::spec::{AuthKind, ProviderSpec, Wire};
use super::stream::{BedrockStream, EventStream, MockStream, SseStream};
use crate::error::{Error, Result};
use serde_json::json;
use std::time::Duration;

/// Response-format enforcement requested for the race.
#[derive(Debug, Clone, PartialEq)]
pub enum JsonOutput {
    /// Any valid JSON value (`json_object`, `responseMimeType`).
    Object,
    /// A JSON Schema the answer must follow (`json_schema` strict on
    /// OpenAI wires, `responseSchema` on Gemini wires).
    Schema(serde_json::Value),
}

/// Optional sampling controls shared by every wire.
///
/// Unset fields are omitted from the request, so a race that does not pass
/// them measures the provider default — the same request earlier versions
/// sent. Set fields are encoded per wire (`top_p` vs Cohere's `p`, a system
/// message vs Anthropic's `system` field vs Gemini's `systemInstruction`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Sampling {
    /// Passed through as `temperature` where the wire has one.
    pub temperature: Option<f64>,
    /// Nucleus sampling (`top_p`, or Cohere's `p`).
    pub top_p: Option<f64>,
    /// Deterministic seed. Sent only on wires that document one
    /// (OpenAI-compatible and Gemini). Other wires ignore it instead of
    /// rejecting the request.
    pub seed: Option<u64>,
    /// System prompt, encoded in the wire's native slot.
    pub system: Option<String>,
    /// Response-format enforcement. Encoded on the OpenAI chat, OpenAI
    /// Responses, Cohere and Gemini wires; the other wires ignore it.
    pub json_output: Option<JsonOutput>,
}

impl Sampling {
    /// `true` when every control is unset or the system prompt is blank.
    #[must_use]
    pub fn is_unset(&self) -> bool {
        self.temperature.is_none()
            && self.top_p.is_none()
            && self.seed.is_none()
            && self.system.as_deref().is_none_or(str::is_empty)
            && self.json_output.is_none()
    }

    /// Rejects values no major provider will accept.
    ///
    /// Temperature uses the OpenAI/Gemini range `0.0..=2.0`. Anthropic's
    /// tighter `0..=1` is left to the provider, which answers HTTP 400
    /// with its own message. `top_p` is `0.0..=1.0`.
    pub fn checked(self) -> std::result::Result<Self, String> {
        if let Some(t) = self.temperature {
            if !t.is_finite() || !(0.0..=2.0).contains(&t) {
                return Err(format!(
                    "temperature must be within 0.0..=2.0 (Anthropic only accepts 0..=1), got {t}"
                ));
            }
        }
        if let Some(p) = self.top_p {
            if !p.is_finite() || !(0.0..=1.0).contains(&p) {
                return Err(format!("top_p must be within 0.0..=1.0, got {p}"));
            }
        }
        if let Some(sys) = &self.system {
            if sys.trim().is_empty() {
                return Err("system prompt is empty".into());
            }
            if sys.chars().count() > 100_000 {
                return Err("system prompt exceeds 100000 characters".into());
            }
        }
        if let Some(JsonOutput::Schema(schema)) = &self.json_output {
            if !schema.is_object() {
                return Err("response schema must be a JSON object".into());
            }
        }
        Ok(self)
    }
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
    /// Which tool task to attach (`crate::tools::registry` name).
    pub tool_task: Option<&'a str>,
    /// Sampling controls. Use [`Sampling::default`] to omit all of them.
    pub sampling: Sampling,
    /// Conversation turns. Empty sends [`Self::prompt`] as one user turn.
    pub messages: &'a [crate::workload::WorkloadMessage],
    /// A workload-defined tool. Wins over [`Self::tool_task`] when set.
    pub tool: Option<&'a crate::tools::ToolDefinition>,
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

/// Percent-encodes a model id for use as a single URL path segment.
///
/// Unreserved characters (RFC 3986) pass through. Everything else,
/// including the `:` in Bedrock model ids, is encoded so the path cannot
/// be split by a reserved character.
fn encode_model_id(model: &str) -> String {
    let mut out = String::with_capacity(model.len());
    for b in model.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn expand_endpoint(endpoint: &str, model: &str) -> String {
    endpoint.replace("{model}", &encode_model_id(model))
}

/// JSON body for one completion. Pure: no credentials, no I/O.
fn build_body(spec: &ProviderSpec, req: &CompletionRequest<'_>) -> serde_json::Value {
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
        Wire::OpenAiResponses => json!({
            "model": req.model,
            "input": [{
                "role": "user",
                "content": [{"type": "input_text", "text": req.prompt}],
            }],
            "stream": true,
            "max_output_tokens": req.max_tokens,
        }),
        Wire::Anthropic | Wire::Cohere => json!({
            "model": req.model,
            "max_tokens": req.max_tokens,
            "messages": [{"role": "user", "content": req.prompt}],
            "stream": true,
        }),
        // Vertex speaks the Gemini streaming wire; the OAuth token is
        // attached at send time, not in the body.
        Wire::Gemini | Wire::Vertex => json!({
            "contents": [{"role": "user", "parts": [{"text": req.prompt}]}],
            "generationConfig": {"maxOutputTokens": req.max_tokens},
        }),
        // Converse is model-agnostic: one document works for Anthropic,
        // Amazon, Meta and Mistral on Bedrock. The decoder already reads
        // `contentBlockDelta` / `metadata` from this operation.
        Wire::Bedrock => json!({
            "messages": [{"role": "user", "content": [{"text": req.prompt}]}],
            "inferenceConfig": {"maxTokens": req.max_tokens},
        }),
    };
    apply_messages(&mut body, spec.wire, req);
    apply_sampling(&mut body, spec.wire, req.model, &req.sampling);
    if let Some(json_output) = &req.sampling.json_output {
        apply_json_output(&mut body, spec.wire, json_output);
    }
    if let Some(tool) = req.tool {
        attach_tool(&mut body, spec.wire, tool);
    } else if req.tools {
        let task = req
            .tool_task
            .and_then(crate::tools::find)
            .unwrap_or_else(default_tool_task);
        attach_tool(
            &mut body,
            spec.wire,
            &crate::tools::ToolDefinition {
                name: task.function.to_string(),
                description: task.description.to_string(),
                parameters: (task.parameters)(),
            },
        );
    }
    body
}

fn apply_messages(body: &mut serde_json::Value, wire: Wire, req: &CompletionRequest<'_>) {
    if req.messages.is_empty() {
        return;
    }
    let turns = req.messages.iter().map(|message| {
        let role = message.role.as_str();
        match wire {
            Wire::Gemini | Wire::Vertex => {
                let role = if role == "assistant" { "model" } else { role };
                json!({"role": role, "parts": [{"text": message.content}]})
            }
            Wire::Bedrock => {
                json!({"role": role, "content": [{"text": message.content}]})
            }
            Wire::OpenAiResponses => {
                let part = if role == "assistant" {
                    "output_text"
                } else {
                    "input_text"
                };
                json!({"role": role, "content": [{"type": part, "text": message.content}]})
            }
            _ => json!({"role": role, "content": message.content}),
        }
    });
    match wire {
        Wire::Mock => {}
        Wire::Gemini | Wire::Vertex => body["contents"] = serde_json::Value::Array(turns.collect()),
        Wire::OpenAiResponses => body["input"] = serde_json::Value::Array(turns.collect()),
        _ => body["messages"] = serde_json::Value::Array(turns.collect()),
    }
}

/// Writes response-format enforcement in each wire's native shape.
///
/// Anthropic, Bedrock and the mock have no such field; the control is
/// theirs to ignore, and the request hash records that it was requested.
fn apply_json_output(body: &mut serde_json::Value, wire: Wire, json_output: &JsonOutput) {
    match (wire, json_output) {
        (Wire::OpenAiCompat | Wire::Cohere, JsonOutput::Object) => {
            body["response_format"] = json!({"type": "json_object"});
        }
        (Wire::OpenAiCompat | Wire::Cohere, JsonOutput::Schema(schema)) => {
            body["response_format"] = json!({
                "type": "json_schema",
                "json_schema": {
                    "name": "llmrace_output",
                    "schema": schema,
                    "strict": true,
                },
            });
        }
        (Wire::OpenAiResponses, JsonOutput::Object) => {
            body["text"]["format"] = json!({"type": "json_object"});
        }
        (Wire::OpenAiResponses, JsonOutput::Schema(schema)) => {
            body["text"]["format"] = json!({
                "type": "json_schema",
                "name": "llmrace_output",
                "schema": schema,
                "strict": true,
            });
        }
        (Wire::Gemini | Wire::Vertex, JsonOutput::Object) => {
            body["generationConfig"]["responseMimeType"] = json!("application/json");
        }
        (Wire::Gemini | Wire::Vertex, JsonOutput::Schema(schema)) => {
            body["generationConfig"]["responseMimeType"] = json!("application/json");
            body["generationConfig"]["responseSchema"] = schema.clone();
        }
        _ => {}
    }
}

fn attach_tool(body: &mut serde_json::Value, wire: Wire, tool: &crate::tools::ToolDefinition) {
    match wire {
        Wire::Mock => {}
        Wire::Bedrock => body["toolConfig"] = crate::tools::tool_body(wire, tool),
        _ => body["tools"] = crate::tools::tool_body(wire, tool),
    }
}

/// Writes sampling controls in each wire's native shape.
///
/// OpenAI reasoning models (gpt-5, o-series) reject `temperature`,
/// `top_p` and `seed`; those fields are omitted so a mixed race can still
/// pin sampling for the classic models beside them. The system prompt is
/// still sent — those models accept it.
fn apply_sampling(body: &mut serde_json::Value, wire: Wire, model: &str, sampling: &Sampling) {
    if sampling.is_unset() {
        return;
    }
    let reasoning = matches!(wire, Wire::OpenAiCompat | Wire::OpenAiResponses)
        && requires_completion_tokens(model);
    match wire {
        Wire::Mock => {}
        Wire::OpenAiResponses => {
            if !reasoning {
                if let Some(t) = sampling.temperature {
                    body["temperature"] = json!(t);
                }
                if let Some(p) = sampling.top_p {
                    body["top_p"] = json!(p);
                }
            }
            // The Responses API has no seed field; a pinned seed lives in
            // the request hash but is not sent on this wire.
            if let Some(sys) = sampling.system.as_deref() {
                body["instructions"] = json!(sys);
            }
        }
        Wire::OpenAiCompat => {
            if !reasoning {
                if let Some(t) = sampling.temperature {
                    body["temperature"] = json!(t);
                }
                if let Some(p) = sampling.top_p {
                    body["top_p"] = json!(p);
                }
                if let Some(seed) = sampling.seed {
                    body["seed"] = json!(seed);
                }
            }
            prepend_system_message(body, sampling.system.as_deref());
        }
        Wire::Anthropic => {
            if let Some(t) = sampling.temperature {
                body["temperature"] = json!(t);
            }
            if let Some(p) = sampling.top_p {
                body["top_p"] = json!(p);
            }
            if let Some(sys) = sampling.system.as_deref() {
                body["system"] = json!(sys);
            }
        }
        Wire::Cohere => {
            if let Some(t) = sampling.temperature {
                body["temperature"] = json!(t);
            }
            if let Some(p) = sampling.top_p {
                body["p"] = json!(p);
            }
            prepend_system_message(body, sampling.system.as_deref());
        }
        Wire::Gemini | Wire::Vertex => {
            if let Some(t) = sampling.temperature {
                body["generationConfig"]["temperature"] = json!(t);
            }
            if let Some(p) = sampling.top_p {
                body["generationConfig"]["topP"] = json!(p);
            }
            if let Some(seed) = sampling.seed {
                body["generationConfig"]["seed"] = json!(seed);
            }
            if let Some(sys) = sampling.system.as_deref() {
                body["systemInstruction"] = json!({"parts": [{"text": sys}]});
            }
        }
        Wire::Bedrock => {
            if let Some(t) = sampling.temperature {
                body["inferenceConfig"]["temperature"] = json!(t);
            }
            if let Some(p) = sampling.top_p {
                body["inferenceConfig"]["topP"] = json!(p);
            }
            if let Some(sys) = sampling.system.as_deref() {
                body["system"] = json!([{"text": sys}]);
            }
        }
    }
}

fn prepend_system_message(body: &mut serde_json::Value, system: Option<&str>) {
    let Some(sys) = system.filter(|s| !s.is_empty()) else {
        return;
    };
    if let Some(messages) = body.get_mut("messages").and_then(|m| m.as_array_mut()) {
        messages.insert(0, json!({"role": "system", "content": sys}));
    }
}

/// Builds the request for the given wire protocol; pure, no I/O.
fn build_wire_request(spec: &ProviderSpec, req: &CompletionRequest<'_>) -> Result<WireRequest> {
    let body = build_body(spec, req);
    let url = expand_endpoint(&spec.endpoint, req.model);
    if spec.wire == Wire::Bedrock || spec.wire == Wire::Vertex {
        // Bedrock signs at send time; Vertex fetches an OAuth token then.
        return Ok(WireRequest {
            url,
            body,
            auth: RequestAuth::None,
        });
    }
    let key = spec.api_key()?;
    let auth = match (spec.wire, spec.auth) {
        (Wire::Mock, _) => RequestAuth::None,
        (Wire::OpenAiCompat, AuthKind::ApiKeyHeader)
        | (Wire::OpenAiResponses, AuthKind::ApiKeyHeader) => {
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
/// let req = CompletionRequest { model: "mock-1", prompt: "hi", max_tokens: 8, tools: false, tool_task: None, sampling: llmrace::provider::Sampling::default(), messages: &[], tool: None };
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
        Wire::OpenAiResponses => (models_url("models", false)?, bearer_or_none(key)),
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

fn parse_retry_after(value: &reqwest::header::HeaderValue) -> Option<std::time::Duration> {
    let text = value.to_str().ok()?.trim();
    if let Ok(secs) = text.parse::<u64>() {
        return Some(std::time::Duration::from_secs(secs));
    }
    let secs: f64 = text.parse().ok()?;
    if secs.is_finite() && secs >= 0.0 {
        Some(std::time::Duration::from_secs_f64(secs))
    } else {
        None
    }
}

async fn check_status(spec: &ProviderSpec, resp: reqwest::Response) -> Result<reqwest::Response> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let retry_after = resp
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(parse_retry_after);
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
        retry_after,
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
        .header("accept", "application/vnd.amazon.eventstream")
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
    let token = super::gcp_oauth::token(client, &account, now).await?;
    let resp = client
        .post(&wire_req.url)
        .bearer_auth(&token)
        .json(&wire_req.body)
        .send()
        .await
        .map_err(|e| Error::Stream(format!("{}: {e}", spec.id)))?;
    let resp = check_status(spec, resp).await?;
    Ok(EventStream::Http(SseStream::new(resp, Wire::Gemini)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req<'a>(model: &'a str, sampling: Sampling) -> CompletionRequest<'a> {
        CompletionRequest {
            model,
            prompt: "hi",
            max_tokens: 8,
            tools: false,
            tool_task: None,
            sampling,
            messages: &[],
            tool: None,
        }
    }

    fn pinned() -> Sampling {
        Sampling {
            temperature: Some(0.0),
            top_p: Some(1.0),
            seed: Some(7),
            system: Some("Be brief.".into()),
            json_output: None,
        }
    }

    #[test]
    fn unset_sampling_omits_every_control() {
        let spec = ProviderSpec::custom("vllm", "http://localhost/v1/chat/completions", None, "m");
        let body = build_body(&spec, &req("gpt-4o-mini", Sampling::default()));
        assert!(body.get("temperature").is_none(), "{body}");
        assert!(body.get("top_p").is_none(), "{body}");
        assert!(body.get("seed").is_none(), "{body}");
        assert_eq!(body["messages"].as_array().map(Vec::len), Some(1));
    }

    #[test]
    fn openai_sampling_is_native_and_system_is_prepended() {
        let spec = ProviderSpec::custom("vllm", "http://localhost/v1/chat/completions", None, "m");
        let body = build_body(&spec, &req("gpt-4o-mini", pinned()));
        assert_eq!(body["temperature"].as_f64(), Some(0.0));
        assert_eq!(body["top_p"].as_f64(), Some(1.0));
        assert_eq!(body["seed"].as_u64(), Some(7));
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][0]["content"], "Be brief.");
        assert_eq!(body["messages"][1]["role"], "user");
    }

    #[test]
    fn reasoning_models_keep_the_system_prompt_and_drop_sampling() {
        let spec = ProviderSpec::custom("vllm", "http://localhost/v1/chat/completions", None, "m");
        let body = build_body(&spec, &req("gpt-5-nano", pinned()));
        assert!(body.get("temperature").is_none(), "{body}");
        assert!(body.get("top_p").is_none(), "{body}");
        assert!(body.get("seed").is_none(), "{body}");
        assert_eq!(body["max_completion_tokens"], 8);
        assert!(body.get("max_tokens").is_none(), "{body}");
        assert_eq!(body["messages"][0]["content"], "Be brief.");
    }

    #[test]
    fn anthropic_uses_top_level_system_and_ignores_seed() {
        let spec = ProviderSpec::anthropic();
        let body = build_body(&spec, &req("claude-3-5-haiku-latest", pinned()));
        assert_eq!(body["temperature"].as_f64(), Some(0.0));
        assert_eq!(body["top_p"].as_f64(), Some(1.0));
        assert!(body.get("seed").is_none(), "{body}");
        assert_eq!(body["system"], "Be brief.");
        assert_eq!(body["messages"].as_array().map(Vec::len), Some(1));
    }

    #[test]
    fn cohere_calls_nucleus_sampling_p() {
        let spec = ProviderSpec::cohere();
        let body = build_body(&spec, &req("command-a-03-2025", pinned()));
        assert_eq!(body["p"].as_f64(), Some(1.0));
        assert!(body.get("top_p").is_none(), "{body}");
        assert_eq!(body["messages"][0]["role"], "system");
    }

    #[test]
    fn gemini_and_vertex_share_generation_config() {
        let sampling = pinned();
        for spec in [ProviderSpec::gemini(), ProviderSpec::vertex()] {
            let body = build_body(&spec, &req("gemini-2.0-flash", sampling.clone()));
            assert_eq!(body["generationConfig"]["maxOutputTokens"], 8);
            assert_eq!(body["generationConfig"]["temperature"].as_f64(), Some(0.0));
            assert_eq!(body["generationConfig"]["topP"].as_f64(), Some(1.0));
            assert_eq!(body["generationConfig"]["seed"].as_u64(), Some(7));
            assert_eq!(body["systemInstruction"]["parts"][0]["text"], "Be brief.");
            assert!(body.get("contents").is_some(), "{body}");
        }
    }

    #[test]
    fn bedrock_converse_body_encodes_the_model_id() {
        let spec = ProviderSpec::bedrock();
        let body = build_body(
            &spec,
            &req("anthropic.claude-3-5-haiku-20241022-v1:0", pinned()),
        );
        assert_eq!(body["inferenceConfig"]["maxTokens"], 8);
        assert_eq!(body["inferenceConfig"]["temperature"].as_f64(), Some(0.0));
        assert_eq!(body["inferenceConfig"]["topP"].as_f64(), Some(1.0));
        assert!(body.get("seed").is_none(), "{body}");
        assert_eq!(body["system"][0]["text"], "Be brief.");
        assert_eq!(body["messages"][0]["content"][0]["text"], "hi");
        let url = expand_endpoint(&spec.endpoint, "anthropic.claude-3-5-haiku-20241022-v1:0");
        assert!(url.contains("/converse-stream"), "{url}");
        assert!(url.contains("%3A"), "{url}");
        assert!(!url.contains("invoke-with-response-stream"), "{url}");
    }

    #[test]
    fn messages_and_a_custom_tool_are_encoded_on_each_wire() {
        use crate::tools::ToolDefinition;
        use crate::workload::{MessageRole, WorkloadMessage};
        let messages = [
            WorkloadMessage {
                role: MessageRole::User,
                content: "What is the capital?".into(),
            },
            WorkloadMessage {
                role: MessageRole::Assistant,
                content: "Of which country?".into(),
            },
            WorkloadMessage {
                role: MessageRole::User,
                content: "France.".into(),
            },
        ];
        let tool = ToolDefinition {
            name: "get_weather".into(),
            description: "lookup".into(),
            parameters: json!({"type": "object", "required": ["city"]}),
        };
        let mut request = req("m", Sampling::default());
        request.messages = &messages;
        request.tool = Some(&tool);
        request.tools = true;

        let openai = build_body(&ProviderSpec::openai(), &request);
        assert_eq!(openai["messages"].as_array().map(Vec::len), Some(3));
        assert_eq!(openai["messages"][2]["content"], "France.");
        assert_eq!(openai["tools"][0]["function"]["name"], "get_weather");

        let gemini = build_body(&ProviderSpec::gemini(), &request);
        assert_eq!(gemini["contents"][1]["role"], "model");

        let bedrock = build_body(&ProviderSpec::bedrock(), &request);
        assert_eq!(bedrock["messages"][1]["role"], "assistant");
        assert_eq!(
            bedrock["toolConfig"]["tools"][0]["toolSpec"]["name"],
            "get_weather"
        );
    }

    #[test]
    fn responses_body_uses_input_instructions_and_flat_tools() {
        use crate::workload::{MessageRole, WorkloadMessage};
        let spec = ProviderSpec::openai_responses();
        let mut request = req("gpt-4o-mini", pinned());
        assert!(spec.opt_in, "the responses lane must be opt-in");
        let body = build_body(&spec, &request);
        assert_eq!(body["input"][0]["content"][0]["type"], "input_text");
        assert_eq!(body["input"][0]["content"][0]["text"], "hi");
        assert_eq!(body["max_output_tokens"], 8);
        assert_eq!(body["instructions"], "Be brief.");
        assert_eq!(body["temperature"].as_f64(), Some(0.0));
        assert!(body.get("seed").is_none(), "{body}");
        assert!(body.get("messages").is_none(), "{body}");

        let conversation = [
            WorkloadMessage {
                role: MessageRole::User,
                content: "Capital?".into(),
            },
            WorkloadMessage {
                role: MessageRole::Assistant,
                content: "Of where?".into(),
            },
            WorkloadMessage {
                role: MessageRole::User,
                content: "France.".into(),
            },
        ];
        request.messages = &conversation;
        let body = build_body(&spec, &request);
        assert_eq!(body["input"].as_array().map(Vec::len), Some(3));
        assert_eq!(body["input"][1]["content"][0]["type"], "output_text");

        let tool = crate::tools::ToolDefinition {
            name: "get_weather".into(),
            description: "lookup".into(),
            parameters: json!({"type": "object", "required": ["city"]}),
        };
        request.tool = Some(&tool);
        let body = build_body(&spec, &request);
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["name"], "get_weather");
        assert!(body["tools"][0].get("function").is_none(), "{body}");

        let reasoning = build_body(&spec, &req("gpt-5-nano", pinned()));
        assert!(reasoning.get("temperature").is_none(), "{reasoning}");
        assert_eq!(reasoning["instructions"], "Be brief.");
    }

    #[test]
    fn json_output_is_encoded_on_each_supporting_wire() {
        let schema = json!({"type": "object", "required": ["city"]});
        let mut sampling = Sampling {
            json_output: Some(JsonOutput::Object),
            ..Sampling::default()
        };
        let mut request = req("gpt-4o-mini", sampling.clone());

        let body = build_body(&ProviderSpec::openai(), &request);
        assert_eq!(body["response_format"]["type"], "json_object");
        let body = build_body(&ProviderSpec::openai_responses(), &request);
        assert_eq!(body["text"]["format"]["type"], "json_object");
        let body = build_body(&ProviderSpec::gemini(), &request);
        assert_eq!(
            body["generationConfig"]["responseMimeType"],
            "application/json"
        );
        assert!(body["generationConfig"].get("responseSchema").is_none());
        let body = build_body(&ProviderSpec::anthropic(), &request);
        assert!(body.get("response_format").is_none(), "{body}");

        sampling.json_output = Some(JsonOutput::Schema(schema.clone()));
        request.sampling = sampling;
        let body = build_body(&ProviderSpec::openai(), &request);
        assert_eq!(body["response_format"]["type"], "json_schema");
        assert_eq!(
            body["response_format"]["json_schema"]["schema"]["required"][0],
            "city"
        );
        assert_eq!(body["response_format"]["json_schema"]["strict"], true);
        let body = build_body(&ProviderSpec::openai_responses(), &request);
        assert_eq!(body["text"]["format"]["type"], "json_schema");
        assert_eq!(body["text"]["format"]["schema"]["required"][0], "city");
        let body = build_body(&ProviderSpec::gemini(), &request);
        assert_eq!(
            body["generationConfig"]["responseMimeType"],
            "application/json"
        );
        assert_eq!(
            body["generationConfig"]["responseSchema"]["required"][0],
            "city"
        );

        let not_object = Sampling {
            json_output: Some(JsonOutput::Schema(json!("array"))),
            ..Sampling::default()
        };
        assert!(not_object.checked().is_err());
    }

    #[test]
    fn sampling_checked_rejects_out_of_range_values() {
        let hot = Sampling {
            temperature: Some(2.5),
            ..Sampling::default()
        };
        assert!(hot.checked().is_err());
        let top = Sampling {
            top_p: Some(1.1),
            ..Sampling::default()
        };
        assert!(top.checked().is_err());
        let blank = Sampling {
            system: Some("   ".into()),
            ..Sampling::default()
        };
        assert!(blank.checked().is_err());
        assert!(pinned().checked().is_ok());
    }

    #[test]
    fn retry_after_parses_delay_seconds_only() {
        let secs = reqwest::header::HeaderValue::from_static("3");
        assert_eq!(
            parse_retry_after(&secs),
            Some(std::time::Duration::from_secs(3))
        );
        let frac = reqwest::header::HeaderValue::from_static("1.5");
        assert_eq!(
            parse_retry_after(&frac),
            Some(std::time::Duration::from_secs_f64(1.5))
        );
        let date = reqwest::header::HeaderValue::from_static("Wed, 21 Oct 2015 07:28:00 GMT");
        assert!(parse_retry_after(&date).is_none());
    }
}
