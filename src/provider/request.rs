//! Request building and dispatch: turns a [`CompletionRequest`] into a
//! wire-specific HTTP call and hands back the decoded event stream.

use super::spec::{AuthKind, ProviderSpec, Wire};
use super::stream::{BedrockStream, EventStream, MockStream, SseStream};
use crate::error::{Error, Result};
use serde_json::json;
use std::time::Duration;

const MAX_RESPONSE_SCHEMA_BYTES: usize = 256 * 1024;

/// Response-format enforcement requested for the race.
#[derive(Debug, Clone, PartialEq)]
pub enum JsonOutput {
    /// Any valid JSON value (`json_object`, `responseMimeType`).
    Object,
    /// A JSON Schema the answer must follow (`json_schema` strict on
    /// OpenAI wires, `output_config.format` on Anthropic, `response_format`
    /// with `json_object` + `schema` on Cohere, and `responseSchema` on
    /// Gemini/Vertex wires).
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
    /// Responses, Cohere (`response_format` with `json_object` + optional
    /// `schema`), Gemini/Vertex, and Anthropic (`output_config.format` for
    /// schemas; Anthropic has no schema-less `json_object` mode) wires;
    /// Bedrock and Mock ignore it.
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
            if serde_json::to_vec(schema)
                .map(|b| b.len())
                .unwrap_or(usize::MAX)
                > MAX_RESPONSE_SCHEMA_BYTES
            {
                return Err(format!(
                    "response schema exceeds {MAX_RESPONSE_SCHEMA_BYTES} bytes"
                ));
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
#[derive(Clone)]
enum RequestAuth {
    /// No key required (local daemons, mock).
    None,
    /// `Authorization: Bearer <key>`.
    Bearer(String),
    /// Extra headers carrying the key (e.g. `x-api-key`, `x-goog-api-key`).
    Headers(Vec<(&'static str, String)>),
}

impl std::fmt::Debug for RequestAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RequestAuth::None => f.write_str("None"),
            RequestAuth::Bearer(_) => f.write_str("Bearer(<redacted>)"),
            RequestAuth::Headers(headers) => {
                let mut list = f.debug_list();
                for (name, _) in headers {
                    list.entry(&(name, "<redacted>"));
                }
                list.finish()
            }
        }
    }
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
/// OpenAI Chat uses `response_format`, OpenAI Responses uses `text.format`,
/// Cohere uses `response_format` (`json_object`, with a top-level `schema`
/// when one is provided), Gemini/Vertex use `generationConfig.responseMimeType`
/// (and `responseSchema`), and Anthropic uses `output_config.format` for
/// schemas (Anthropic has no schema-less `json_object` mode). Bedrock and the
/// mock wire ignore the control; the request hash still records that it was
/// requested.
fn apply_json_output(body: &mut serde_json::Value, wire: Wire, json_output: &JsonOutput) {
    match (wire, json_output) {
        (Wire::OpenAiCompat | Wire::Cohere, JsonOutput::Object) => {
            body["response_format"] = json!({"type": "json_object"});
        }
        (Wire::OpenAiCompat, JsonOutput::Schema(schema)) => {
            body["response_format"] = json!({
                "type": "json_schema",
                "json_schema": {
                    "name": "llmrace_output",
                    "schema": schema,
                    "strict": true,
                },
            });
        }
        (Wire::Cohere, JsonOutput::Schema(schema)) => {
            body["response_format"] = json!({
                "type": "json_object",
                "schema": schema,
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
        (Wire::Anthropic, JsonOutput::Schema(schema)) => {
            body["output_config"]["format"] = json!({
                "type": "json_schema",
                "schema": schema,
            });
        }
        (Wire::Gemini | Wire::Vertex, JsonOutput::Object) => {
            body["generationConfig"]["responseMimeType"] = json!("application/json");
        }
        (Wire::Gemini | Wire::Vertex, JsonOutput::Schema(schema)) => {
            body["generationConfig"]["responseMimeType"] = json!("application/json");
            body["generationConfig"]["responseSchema"] = schema.clone();
        }
        (Wire::Anthropic, JsonOutput::Object) | (Wire::Bedrock | Wire::Mock, _) => {}
    }
}

fn attach_tool(body: &mut serde_json::Value, wire: Wire, tool: &crate::tools::ToolDefinition) {
    match wire {
        Wire::Mock => {}
        Wire::Bedrock => body["toolConfig"] = crate::tools::tool_body(wire, tool),
        _ => body["tools"] = crate::tools::tool_body(wire, tool),
    }
}

fn is_claude_model(model: &str) -> bool {
    let lower = model.to_ascii_lowercase();
    lower.contains("claude") || lower.contains("anthropic")
}

/// Writes sampling controls in each wire's native shape.
///
/// OpenAI reasoning models (gpt-5, o-series) reject `temperature`,
/// `top_p` and `seed`; those fields are omitted so a mixed race can still
/// pin sampling for the classic models beside them. The system prompt is
/// still sent — those models accept it. Anthropic (and Claude on Bedrock)
/// reject sending both `temperature` and `top_p` together, so `top_p` is
/// omitted when `temperature` is set.
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
            if sampling.temperature.is_none() {
                if let Some(p) = sampling.top_p {
                    body["top_p"] = json!(p);
                }
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
            if !(sampling.temperature.is_some() && is_claude_model(model)) {
                if let Some(p) = sampling.top_p {
                    body["inferenceConfig"]["topP"] = json!(p);
                }
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

const MAX_RETRY_AFTER_SECS: u64 = 30;
const MAX_RETRY_AFTER_SECS_F64: f64 = 30.0;
const MAX_ERROR_BODY_BYTES: usize = 16 * 1024;
const MAX_MODELS_BODY_BYTES: usize = 4 * 1024 * 1024;

fn is_sensitive_header(name: &str) -> bool {
    matches!(
        name,
        "authorization" | "x-api-key" | "x-goog-api-key" | "api-key" | "x-amz-security-token"
    )
}

fn header_value(name: &str, value: &str) -> Result<reqwest::header::HeaderValue> {
    let mut hv = reqwest::header::HeaderValue::from_str(value)
        .map_err(|_| Error::Config(format!("invalid header value for {name}")))?;
    if is_sensitive_header(name) {
        hv.set_sensitive(true);
    }
    Ok(hv)
}

fn apply_auth(
    mut request: reqwest::RequestBuilder,
    auth: &RequestAuth,
) -> Result<reqwest::RequestBuilder> {
    match auth {
        RequestAuth::None => {}
        RequestAuth::Bearer(key) => {
            let hv = header_value("authorization", &format!("Bearer {key}"))?;
            request = request.header(reqwest::header::AUTHORIZATION, hv);
        }
        RequestAuth::Headers(headers) => {
            for (name, value) in headers {
                let hv = header_value(name, value)?;
                request = request.header(*name, hv);
            }
        }
    }
    Ok(request)
}

/// Formats a `reqwest::Error` without leaking URLs (which may contain query
/// tokens) and with a `"request timed out: "` prefix when `e.is_timeout()` so
/// [`crate::bench::FailureClass::of`] reliably classifies timeouts.
pub(super) fn format_reqwest_error(prefix: Option<&str>, e: reqwest::Error) -> String {
    let timeout = e.is_timeout();
    let clean = e.without_url();
    match (prefix, timeout) {
        (Some(p), true) => format!("{p}: request timed out: {clean}"),
        (Some(p), false) => format!("{p}: {clean}"),
        (None, true) => format!("request timed out: {clean}"),
        (None, false) => clean.to_string(),
    }
}

fn openai_auth(spec: &ProviderSpec, key: Option<String>) -> RequestAuth {
    match (spec.auth, key) {
        (AuthKind::ApiKeyHeader, Some(k)) => RequestAuth::Headers(vec![("api-key", k)]),
        (AuthKind::ApiKeyHeader, None) => RequestAuth::None,
        (AuthKind::Bearer, Some(k)) => RequestAuth::Bearer(k),
        (AuthKind::Bearer, None) => RequestAuth::None,
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
    let auth = match spec.wire {
        Wire::Mock => RequestAuth::None,
        Wire::OpenAiCompat | Wire::OpenAiResponses => openai_auth(spec, key),
        Wire::Anthropic => RequestAuth::Headers(vec![
            ("x-api-key", key.clone().unwrap_or_default()),
            ("anthropic-version", "2023-06-01".to_string()),
        ]),
        Wire::Gemini => {
            RequestAuth::Headers(vec![("x-goog-api-key", key.clone().unwrap_or_default())])
        }
        _ => bearer_or_none(key),
    };
    Ok(WireRequest { url, body, auth })
}

/// Sends a built wire request and returns the decoded SSE stream.
async fn send_wire_request(
    client: &reqwest::Client,
    spec: &ProviderSpec,
    wire_req: WireRequest,
) -> Result<EventStream> {
    if spec.wire == Wire::Bedrock {
        return send_bedrock(client, spec, wire_req).await;
    }
    if spec.wire == Wire::Vertex {
        return send_vertex(client, spec, wire_req).await;
    }
    let request = apply_auth(
        client.post(&wire_req.url).json(&wire_req.body),
        &wire_req.auth,
    )?;
    let resp = request
        .send()
        .await
        .map_err(|e| Error::Stream(format_reqwest_error(None, e)))?;
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

/// URL derivation via `Url::join` — proper RFC 3986 semantics instead of
/// string surgery, so custom endpoints with query strings, ports or trailing
/// paths resolve correctly.
fn models_url_for_spec(spec: &ProviderSpec, suffix: &str, replace_last: bool) -> Result<String> {
    let base = reqwest::Url::parse(&spec.endpoint)
        .map_err(|e| Error::Config(format!("invalid endpoint for {}: {e}", spec.id)))?;
    let mut url = base.clone();
    if replace_last {
        // ".../chat/completions" -> ".../chat" -> join("models") -> ".../models"
        let mut segments = base.path_segments().map(Iterator::collect::<Vec<_>>);
        if let Some(segs) = segments.as_mut() {
            if segs.last() == Some(&"completions") {
                segs.pop();
            }
            url.set_path(&segs.join("/"));
        }
    }
    url.join(suffix)
        .map(|u| u.to_string())
        .map_err(|e| Error::Config(format!("cannot join models path for {}: {e}", spec.id)))
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
    if spec.wire == Wire::Bedrock {
        let region = super::spec::safe_env_segment("AWS_REGION", "us-east-1");
        let url = format!("https://bedrock.{region}.amazonaws.com/foundation-models");
        return list_bedrock_models(client, spec, &url).await;
    }
    let (url, auth) = match spec.wire {
        Wire::Vertex => {
            // Vertex models list uses the regional publisher endpoint with an OAuth bearer token.
            let region = super::spec::safe_env_segment("GOOGLE_CLOUD_REGION", "us-central1");
            let token = vertex_bearer_token(client).await?;
            (
                format!("https://{region}-aiplatform.googleapis.com/v1/publishers/google/models"),
                RequestAuth::Bearer(token),
            )
        }
        Wire::OpenAiCompat => {
            let key = spec.api_key()?;
            (
                models_url_for_spec(spec, "models", true)?,
                openai_auth(spec, key),
            )
        }
        Wire::OpenAiResponses => {
            let key = spec.api_key()?;
            (
                models_url_for_spec(spec, "models", false)?,
                openai_auth(spec, key),
            )
        }
        Wire::Anthropic => {
            let key = spec.api_key()?;
            (
                models_url_for_spec(spec, "models", false)?,
                RequestAuth::Headers(vec![
                    ("x-api-key", key.clone().unwrap_or_default()),
                    ("anthropic-version", "2023-06-01".to_string()),
                ]),
            )
        }
        Wire::Gemini => {
            let key = spec.api_key()?;
            (
                spec.endpoint
                    .split("/models")
                    .next()
                    .unwrap_or(spec.endpoint.as_ref())
                    .trim_end_matches('/')
                    .to_string()
                    + "/models",
                RequestAuth::Headers(vec![("x-goog-api-key", key.clone().unwrap_or_default())]),
            )
        }
        Wire::Cohere => {
            let key = spec.api_key()?;
            (
                spec.endpoint.trim_end_matches("/chat").to_string() + "/models",
                bearer_or_none(key),
            )
        }
        Wire::Bedrock | Wire::Mock => unreachable!("handled above"),
    };
    let request = apply_auth(client.get(&url), &auth)?;
    let resp = request
        .send()
        .await
        .map_err(|e| Error::Stream(format_reqwest_error(Some(&spec.id), e)))?;
    let resp = check_status(spec, resp).await?;
    let raw = crate::util::read_body_capped(resp, MAX_MODELS_BODY_BYTES)
        .await
        .map_err(|e| Error::Stream(format_reqwest_error(Some(&spec.id), e)))?;
    let body: Value = serde_json::from_str(&raw)
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

pub(super) fn parse_retry_after(
    value: &reqwest::header::HeaderValue,
) -> Option<std::time::Duration> {
    let text = value.to_str().ok()?.trim();
    if let Ok(secs) = text.parse::<u64>() {
        return Some(std::time::Duration::from_secs(
            secs.min(MAX_RETRY_AFTER_SECS),
        ));
    }
    let secs: f64 = text.parse().ok()?;
    if secs.is_finite() && secs >= 0.0 {
        std::time::Duration::try_from_secs_f64(secs.min(MAX_RETRY_AFTER_SECS_F64)).ok()
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
    let body = crate::util::read_body_capped(resp, MAX_ERROR_BODY_BYTES)
        .await
        .unwrap_or_default();
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

fn signed_host(url: &reqwest::Url) -> String {
    let host = url.host_str().unwrap_or_default();
    match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    }
}

/// Builds a SigV4-signed Bedrock HTTP request (shared by streaming and model discovery).
fn sign_bedrock_request(
    client: &reqwest::Client,
    method: reqwest::Method,
    url: reqwest::Url,
    body: Vec<u8>,
    accept: &'static str,
) -> Result<reqwest::RequestBuilder> {
    use super::sigv4;

    let creds = sigv4::credentials_from_env().ok_or_else(sigv4::missing_credentials_error)?;
    let region = super::spec::safe_env_segment("AWS_REGION", "us-east-1");
    let host = signed_host(&url);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let signed = sigv4::sign(
        &creds,
        &region,
        &host,
        method.as_str(),
        url.path(),
        url.query().unwrap_or(""),
        &body,
        &sigv4::amz_date(now),
    );
    let mut request = client
        .request(method, url)
        .header("x-amz-date", &signed.amz_date)
        .header("x-amz-content-sha256", &signed.content_sha256)
        .header(
            reqwest::header::AUTHORIZATION,
            header_value("authorization", &signed.authorization)?,
        )
        .header("accept", accept);
    if !body.is_empty() {
        request = request
            .header("content-type", "application/json")
            .body(body);
    }
    if let Some(token) = &signed.session_token {
        request = request.header(
            "x-amz-security-token",
            header_value("x-amz-security-token", token)?,
        );
    }
    Ok(request)
}

/// Sends a SigV4-signed Bedrock request and returns the event-stream.
async fn send_bedrock(
    client: &reqwest::Client,
    spec: &ProviderSpec,
    wire_req: WireRequest,
) -> Result<EventStream> {
    let url = reqwest::Url::parse(&wire_req.url)
        .map_err(|e| Error::Config(format!("invalid bedrock endpoint: {e}")))?;
    let body = serde_json::to_vec(&wire_req.body)
        .map_err(|e| Error::Config(format!("bedrock body encode: {e}")))?;
    let request = sign_bedrock_request(
        client,
        reqwest::Method::POST,
        url,
        body,
        "application/vnd.amazon.eventstream",
    )?;
    let resp = request
        .send()
        .await
        .map_err(|e| Error::Stream(format_reqwest_error(Some(&spec.id), e)))?;
    let resp = check_status(spec, resp).await?;
    Ok(EventStream::Bedrock(BedrockStream::new(resp)))
}

/// GET ListFoundationModels with SigV4, returning model ids.
async fn list_bedrock_models(
    client: &reqwest::Client,
    spec: &ProviderSpec,
    url: &str,
) -> Result<Vec<String>> {
    use serde_json::Value;

    let parsed = reqwest::Url::parse(url)
        .map_err(|e| Error::Config(format!("invalid bedrock models url: {e}")))?;
    let request = sign_bedrock_request(
        client,
        reqwest::Method::GET,
        parsed,
        Vec::new(),
        "application/json",
    )?;
    let resp = request
        .send()
        .await
        .map_err(|e| Error::Stream(format_reqwest_error(Some(&spec.id), e)))?;
    let resp = check_status(spec, resp).await?;
    let raw = crate::util::read_body_capped(resp, MAX_MODELS_BODY_BYTES)
        .await
        .map_err(|e| Error::Stream(format_reqwest_error(Some(&spec.id), e)))?;
    let body: Value = serde_json::from_str(&raw)
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

async fn vertex_bearer_token(client: &reqwest::Client) -> Result<String> {
    let account = super::gcp_oauth::load_service_account_async().await?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    super::gcp_oauth::token(client, &account, now).await
}

/// Fetches an OAuth2 token and sends the Vertex request (Gemini SSE wire).
async fn send_vertex(
    client: &reqwest::Client,
    spec: &ProviderSpec,
    wire_req: WireRequest,
) -> Result<EventStream> {
    let token = vertex_bearer_token(client).await?;
    let request = apply_auth(
        client.post(&wire_req.url).json(&wire_req.body),
        &RequestAuth::Bearer(token),
    )?;
    let resp = request
        .send()
        .await
        .map_err(|e| Error::Stream(format_reqwest_error(Some(&spec.id), e)))?;
    let resp = check_status(spec, resp).await?;
    Ok(EventStream::Http(SseStream::new(resp, spec.wire)))
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
    fn anthropic_uses_top_level_system_ignores_seed_and_omits_top_p_with_temperature() {
        let spec = ProviderSpec::anthropic();
        let body = build_body(&spec, &req("claude-3-5-haiku-latest", pinned()));
        assert_eq!(body["temperature"].as_f64(), Some(0.0));
        assert!(
            body.get("top_p").is_none(),
            "Anthropic rejects temperature + top_p together: {body}"
        );
        assert!(body.get("seed").is_none(), "{body}");
        assert_eq!(body["system"], "Be brief.");
        assert_eq!(body["messages"].as_array().map(Vec::len), Some(1));

        let top_p_only = Sampling {
            top_p: Some(0.9),
            ..Sampling::default()
        };
        let body = build_body(&spec, &req("claude-3-5-haiku-latest", top_p_only));
        assert!(body.get("temperature").is_none());
        assert_eq!(body["top_p"].as_f64(), Some(0.9));
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
        assert!(
            body["inferenceConfig"].get("topP").is_none(),
            "Bedrock Claude omits topP when temperature is set: {body}"
        );
        assert!(body.get("seed").is_none(), "{body}");
        assert_eq!(body["system"][0]["text"], "Be brief.");
        assert_eq!(body["messages"][0]["content"][0]["text"], "hi");
        let url = expand_endpoint(&spec.endpoint, "anthropic.claude-3-5-haiku-20241022-v1:0");
        assert!(url.contains("/converse-stream"), "{url}");
        assert!(url.contains("%3A"), "{url}");
        assert!(!url.contains("invoke-with-response-stream"), "{url}");

        // Non-Claude Bedrock models still receive both temperature and topP.
        let llama = build_body(&spec, &req("meta.llama3-70b-instruct-v1:0", pinned()));
        assert_eq!(llama["inferenceConfig"]["temperature"].as_f64(), Some(0.0));
        assert_eq!(llama["inferenceConfig"]["topP"].as_f64(), Some(1.0));
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
        assert!(body.get("output_config").is_none(), "{body}");

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
        let body = build_body(&ProviderSpec::anthropic(), &request);
        assert_eq!(
            body["output_config"]["format"],
            json!({
                "type": "json_schema",
                "schema": schema,
            })
        );
        assert!(body.get("response_format").is_none(), "{body}");

        let not_object = Sampling {
            json_output: Some(JsonOutput::Schema(json!("array"))),
            ..Sampling::default()
        };
        assert!(not_object.checked().is_err());
    }

    #[test]
    fn anthropic_json_schema_composes_with_reproducible_sampling_and_tools() {
        let schema = json!({
            "type": "object",
            "properties": {"city": {"type": "string"}},
            "required": ["city"],
            "additionalProperties": false
        });
        let reproducible = Sampling {
            temperature: Some(0.0),
            top_p: Some(1.0),
            seed: Some(0),
            system: Some("Be brief.".into()),
            json_output: Some(JsonOutput::Schema(schema.clone())),
        };
        let mut request = req("claude-sonnet-4-5", reproducible);
        request.tools = true;
        let body = build_body(&ProviderSpec::anthropic(), &request);
        assert_eq!(body["temperature"].as_f64(), Some(0.0));
        assert!(
            body.get("top_p").is_none(),
            "Anthropic omits top_p when temperature is set: {body}"
        );
        assert!(body.get("seed").is_none(), "{body}");
        assert_eq!(body["system"], "Be brief.");
        assert_eq!(
            body["output_config"]["format"],
            json!({
                "type": "json_schema",
                "schema": schema,
            })
        );
        assert!(body["tools"].is_array(), "{body}");
        assert_eq!(body["tools"][0]["name"], "get_weather");
    }

    #[test]
    fn sampling_checked_rejects_oversized_response_schema() {
        let provider_schema = Sampling {
            json_output: Some(JsonOutput::Schema(json!({
                "type": "object",
                "$defs": {"item": {"anyOf": [{"type": "string"}, {"type": "null"}]}},
                "properties": {"x": {"$ref": "#/$defs/item"}}
            }))),
            ..Sampling::default()
        };
        assert!(provider_schema.checked().is_ok());

        let oversized = Sampling {
            json_output: Some(JsonOutput::Schema(json!({
                "type": "object",
                "description": "x".repeat(MAX_RESPONSE_SCHEMA_BYTES),
            }))),
            ..Sampling::default()
        };
        assert!(oversized.checked().is_err());
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
        let two_five = reqwest::header::HeaderValue::from_static("2.5");
        assert_eq!(
            parse_retry_after(&two_five),
            Some(std::time::Duration::from_secs_f64(2.5))
        );
        let date = reqwest::header::HeaderValue::from_static("Wed, 21 Oct 2015 07:28:00 GMT");
        assert!(parse_retry_after(&date).is_none());
    }

    #[test]
    fn retry_after_handles_hostile_and_overflow_values_without_panic() {
        let huge_float = reqwest::header::HeaderValue::from_static("1e300");
        assert_eq!(
            parse_retry_after(&huge_float),
            Some(std::time::Duration::from_secs(30))
        );
        let nan = reqwest::header::HeaderValue::from_static("NaN");
        assert!(parse_retry_after(&nan).is_none());
        let neg = reqwest::header::HeaderValue::from_static("-1");
        assert!(parse_retry_after(&neg).is_none());
        let max_u64 = reqwest::header::HeaderValue::from_str(&u64::MAX.to_string()).expect("hv");
        assert_eq!(
            parse_retry_after(&max_u64),
            Some(std::time::Duration::from_secs(30))
        );
    }

    #[test]
    fn cohere_json_schema_uses_json_object_with_top_level_schema() {
        let schema = json!({"type": "object", "required": ["city"]});
        let sampling = Sampling {
            json_output: Some(JsonOutput::Schema(schema)),
            ..Sampling::default()
        };
        let body = build_body(&ProviderSpec::cohere(), &req("command-a-03-2025", sampling));
        assert_eq!(body["response_format"]["type"], "json_object");
        assert_eq!(body["response_format"]["schema"]["required"][0], "city");
        assert!(
            body["response_format"].get("json_schema").is_none(),
            "{body}"
        );
    }

    #[test]
    fn request_auth_debug_redacts_secrets() {
        let bearer = RequestAuth::Bearer("sk-super-secret-123".into());
        let rendered = format!("{bearer:?}");
        assert!(!rendered.contains("sk-super-secret-123"), "{rendered}");
        assert!(rendered.contains("redacted"), "{rendered}");

        let headers = RequestAuth::Headers(vec![("x-api-key", "ant-secret-456".into())]);
        let rendered = format!("{headers:?}");
        assert!(!rendered.contains("ant-secret-456"), "{rendered}");
        assert!(rendered.contains("x-api-key"), "{rendered}");
    }

    #[test]
    fn models_url_resolves_anthropic_openai_and_responses() {
        assert_eq!(
            models_url_for_spec(&ProviderSpec::anthropic(), "models", false).expect("anthropic"),
            "https://api.anthropic.com/v1/models"
        );
        assert_eq!(
            models_url_for_spec(&ProviderSpec::openai(), "models", true).expect("openai"),
            "https://api.openai.com/v1/models"
        );
        assert_eq!(
            models_url_for_spec(&ProviderSpec::openai_responses(), "models", false)
                .expect("responses"),
            "https://api.openai.com/v1/models"
        );
    }

    #[test]
    fn signed_host_includes_non_default_port_only() {
        let default_https =
            reqwest::Url::parse("https://bedrock-runtime.us-east-1.amazonaws.com/x").expect("url");
        assert_eq!(
            signed_host(&default_https),
            "bedrock-runtime.us-east-1.amazonaws.com"
        );
        let custom_port =
            reqwest::Url::parse("http://127.0.0.1:8443/model/m/converse-stream").expect("url");
        assert_eq!(signed_host(&custom_port), "127.0.0.1:8443");
    }
}
