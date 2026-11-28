//! MCP server: llmrace as a tool set for any MCP-capable agent.
//!
//! `llmrace --mcp` speaks newline-delimited JSON-RPC 2.0 on stdio. Four
//! tools: `llmrace_race`, `llmrace_gate`, `llmrace_doctor`, `llmrace_list`.
//! API keys come from the environment; values never appear in responses.

use anyhow::Result;
use llmrace::bench::WarmupMode;
use llmrace::provider::{JsonOutput, ProviderSpec, Sampling};
use llmrace::race::{GatePolicy, Race, DEFAULT_PROMPT};
use llmrace::regression::BaselineReport;
use serde_json::{json, Value};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};

const PROTOCOL_VERSION: &str = "2024-11-05";
const MAX_LINE_BYTES: usize = 1024 * 1024;
const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;

enum LineRead {
    Eof,
    Line(String),
    TooLong,
    InvalidUtf8,
}

/// Reads one newline-delimited frame from `reader`, capping buffered memory at
/// `max_bytes`. Oversized frames are drained to the next `\n` or EOF so the
/// server can report an error and keep serving subsequent frames without OOM.
async fn read_bounded_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    max_bytes: usize,
) -> std::io::Result<LineRead> {
    let mut buf = Vec::new();
    let mut exceeded = false;
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            if exceeded {
                return Ok(LineRead::TooLong);
            }
            if buf.is_empty() {
                return Ok(LineRead::Eof);
            }
            break;
        }
        if let Some(pos) = available.iter().position(|&b| b == b'\n') {
            let take = pos + 1;
            if !exceeded {
                if buf.len().saturating_add(pos) > max_bytes {
                    exceeded = true;
                    buf.clear();
                } else {
                    buf.extend_from_slice(&available[..pos]);
                }
            }
            reader.consume(take);
            if exceeded {
                return Ok(LineRead::TooLong);
            }
            break;
        } else {
            let len = available.len();
            if !exceeded {
                if buf.len().saturating_add(len) > max_bytes {
                    exceeded = true;
                    buf.clear();
                } else {
                    buf.extend_from_slice(available);
                }
            }
            reader.consume(len);
        }
    }
    if buf.last() == Some(&b'\r') {
        buf.pop();
    }
    match String::from_utf8(buf) {
        Ok(line) => Ok(LineRead::Line(line)),
        Err(_) => Ok(LineRead::InvalidUtf8),
    }
}

/// Runs the server until stdin closes.
pub(crate) async fn serve() -> Result<()> {
    let mut reader = BufReader::new(tokio::io::stdin());
    let mut out = tokio::io::stdout();
    loop {
        let line = match read_bounded_line(&mut reader, MAX_LINE_BYTES).await? {
            LineRead::Eof => break,
            LineRead::TooLong => {
                write(
                    &mut out,
                    error(Value::Null, -32600, "request line exceeds 1 MiB limit"),
                )
                .await?;
                continue;
            }
            LineRead::InvalidUtf8 => {
                write(
                    &mut out,
                    error(Value::Null, -32700, "parse error: invalid UTF-8"),
                )
                .await?;
                continue;
            }
            LineRead::Line(line) => line,
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                write(
                    &mut out,
                    error(Value::Null, -32700, &format!("parse error: {e}")),
                )
                .await?;
                continue;
            }
        };
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        let id = msg.get("id").cloned().unwrap_or(Value::Null);
        match method {
            "initialize" => write(&mut out, result(id, initialize_result())).await?,
            "notifications/initialized" | "notifications/cancelled" => {}
            "ping" => write(&mut out, result(id, json!({}))).await?,
            "tools/list" => write(&mut out, result(id, tools_list())).await?,
            "tools/call" => {
                let response = call_tool(id, &msg).await;
                write(&mut out, response).await?;
            }
            _ => {
                if msg.get("id").is_some() {
                    write(
                        &mut out,
                        error(id, -32601, &format!("method not found: {method}")),
                    )
                    .await?;
                }
            }
        }
    }
    Ok(())
}

async fn write(out: &mut tokio::io::Stdout, msg: Value) -> Result<()> {
    out.write_all(msg.to_string().as_bytes()).await?;
    out.write_all(b"\n").await?;
    out.flush().await?;
    Ok(())
}

fn result(id: Value, value: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": value})
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn initialize_result() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": {"tools": {}},
        "serverInfo": {"name": "llmrace", "version": env!("CARGO_PKG_VERSION")},
    })
}

fn text_result(text: String) -> Value {
    json!({"content": [{"type": "text", "text": text}], "isError": false})
}

fn error_result(text: String) -> Value {
    json!({"content": [{"type": "text", "text": text}], "isError": true})
}

fn tools_list() -> Value {
    json!({"tools": [
        {
            "name": "llmrace_race",
            "description": "Race LLM providers head-to-head and return the markdown report. Providers are CLI ids like \"openai\" or \"openai=gpt-4o-mini\"; \"mock\" races offline.",
            "inputSchema": race_schema(),
        },
        {
            "name": "llmrace_gate",
            "description": "Race providers against a saved baseline report and return the regression verdict. Fails (isError) when the gate fails.",
            "inputSchema": gate_schema(),
        },
        {
            "name": "llmrace_doctor",
            "description": "Which providers are configured (API key present) and which are experimental. No key values are returned.",
            "inputSchema": {"type": "object", "properties": {}},
        },
        {
            "name": "llmrace_list",
            "description": "The provider registry: ids, default models, wire protocols, opt-in and experimental flags.",
            "inputSchema": {"type": "object", "properties": {}},
        },
    ]})
}

fn shared_race_properties() -> serde_json::Map<String, Value> {
    let mut props = serde_json::Map::new();
    props.insert(
        "providers".into(),
        json!({"type": "array", "items": {"type": "string"}, "minItems": 1, "maxItems": 64}),
    );
    props.insert("prompt".into(), json!({"type": "string"}));
    props.insert(
        "workload".into(),
        json!({"type": "string", "description": "Path to a JSONL workload file"}),
    );
    props.insert(
        "runs".into(),
        json!({"type": "integer", "minimum": 1, "maximum": 1000}),
    );
    props.insert(
        "warmup".into(),
        json!({"type": "integer", "minimum": 0, "maximum": 100}),
    );
    props.insert(
        "warmup_mode".into(),
        json!({
            "type": "string",
            "enum": ["same-prompt", "probe"],
            "description": "Warmup request mode: same-prompt (default) or probe"
        }),
    );
    props.insert(
        "max_tokens".into(),
        json!({"type": "integer", "minimum": 1, "maximum": 131_072}),
    );
    props.insert(
        "concurrency".into(),
        json!({"type": "integer", "minimum": 1, "maximum": 128}),
    );
    props.insert(
        "retries".into(),
        json!({"type": "integer", "minimum": 0, "maximum": 10}),
    );
    props.insert(
        "timeout_secs".into(),
        json!({"type": "integer", "minimum": 1, "maximum": 600}),
    );
    props.insert(
        "reproducible".into(),
        json!({"type": "boolean", "description": "Pin temperature=0, top_p=1, seed=0"}),
    );
    props.insert(
        "system".into(),
        json!({"type": "string", "description": "System prompt sent with every request"}),
    );
    props.insert(
        "tools".into(),
        json!({"type": "boolean", "description": "Enable built-in tool-calling benchmark (weather task)"}),
    );
    props.insert(
        "json_output".into(),
        json!({"type": "boolean", "description": "Require JSON object output where supported"}),
    );
    props.insert(
        "response_schema".into(),
        json!({
            "type": "object",
            "description": "Inline JSON Schema object to enforce structured output (mutually exclusive with json_output: true)"
        }),
    );
    props
}

fn race_schema() -> Value {
    let mut props = shared_race_properties();
    props.insert(
        "winner".into(),
        json!({"type": "boolean", "description": "Include the weighted podium"}),
    );
    json!({
        "type": "object",
        "required": ["providers"],
        "properties": Value::Object(props),
    })
}

fn gate_schema() -> Value {
    let mut props = shared_race_properties();
    props.insert(
        "baseline".into(),
        json!({"type": "string", "description": "Path to a race.json baseline report"}),
    );
    props.insert(
        "max_regression".into(),
        json!({"type": "number", "minimum": 0, "description": "Threshold in percent (default 15)"}),
    );
    props.insert(
        "require_significance".into(),
        json!({"type": "boolean", "description": "Latency/throughput breaches must be significant (default true)"}),
    );
    props.insert(
        "fail_on_inconclusive".into(),
        json!({"type": "boolean", "description": "Fail breached metrics with no usable significance test (default false)"}),
    );
    json!({
        "type": "object",
        "required": ["providers", "baseline"],
        "properties": Value::Object(props),
    })
}

async fn call_tool(id: Value, msg: &Value) -> Value {
    let params = &msg["params"];
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let args = match params.get("arguments") {
        None | Some(Value::Null) => json!({}),
        Some(v) if v.is_object() => v.clone(),
        Some(_) => {
            return result(id, error_result("'arguments' must be a JSON object".into()));
        }
    };
    let outcome = match name {
        "llmrace_race" => tool_race(&args).await,
        "llmrace_gate" => tool_gate(&args).await,
        "llmrace_doctor" => tool_doctor(),
        "llmrace_list" => tool_list(),
        other => Err(format!("unknown tool '{other}'")),
    };
    match outcome {
        Ok(text) => result(id, text_result(text)),
        Err(message) => result(id, error_result(message)),
    }
}

fn opt_usize_in_range(
    args: &Value,
    key: &str,
    min: usize,
    max: usize,
) -> Result<Option<usize>, String> {
    let Some(value) = args.get(key) else {
        return Ok(None);
    };
    let Some(raw) = value.as_u64() else {
        return Err(format!("'{key}' must be an integer in {min}..={max}"));
    };
    let parsed =
        usize::try_from(raw).map_err(|_| format!("'{key}' must be an integer in {min}..={max}"))?;
    if !(min..=max).contains(&parsed) {
        return Err(format!(
            "'{key}' must be an integer in {min}..={max}, got {parsed}"
        ));
    }
    Ok(Some(parsed))
}

fn opt_u64_in_range(args: &Value, key: &str, min: u64, max: u64) -> Result<Option<u64>, String> {
    let Some(value) = args.get(key) else {
        return Ok(None);
    };
    let Some(parsed) = value.as_u64() else {
        return Err(format!("'{key}' must be an integer in {min}..={max}"));
    };
    if !(min..=max).contains(&parsed) {
        return Err(format!(
            "'{key}' must be an integer in {min}..={max}, got {parsed}"
        ));
    }
    Ok(Some(parsed))
}

fn opt_f64_non_neg(args: &Value, key: &str) -> Result<Option<f64>, String> {
    let Some(value) = args.get(key) else {
        return Ok(None);
    };
    let Some(parsed) = value.as_f64() else {
        return Err(format!("'{key}' must be a finite nonnegative number"));
    };
    if !parsed.is_finite() || parsed < 0.0 {
        return Err(format!("'{key}' must be finite and nonnegative"));
    }
    Ok(Some(parsed))
}

fn opt_bool(args: &Value, key: &str) -> Result<Option<bool>, String> {
    let Some(value) = args.get(key) else {
        return Ok(None);
    };
    value
        .as_bool()
        .map(Some)
        .ok_or_else(|| format!("'{key}' must be a boolean"))
}

fn opt_str<'a>(args: &'a Value, key: &str) -> Result<Option<&'a str>, String> {
    let Some(value) = args.get(key) else {
        return Ok(None);
    };
    value
        .as_str()
        .map(Some)
        .ok_or_else(|| format!("'{key}' must be a string"))
}

fn opt_object(args: &Value, key: &str) -> Result<Option<Value>, String> {
    let Some(value) = args.get(key) else {
        return Ok(None);
    };
    if !value.is_object() {
        return Err(format!("'{key}' must be a JSON object"));
    }
    Ok(Some(value.clone()))
}

async fn read_regular_file_bounded(path: &str, label: &'static str) -> Result<String, String> {
    read_regular_file_with_limit(path, label, MAX_FILE_BYTES).await
}

async fn read_regular_file_with_limit(
    path: &str,
    label: &'static str,
    max_bytes: u64,
) -> Result<String, String> {
    let path_owned = path.to_string();
    tokio::task::spawn_blocking(move || {
        use std::io::Read as _;
        // Stat before open so POSIX FIFOs are rejected without blocking on open().
        let meta = std::fs::metadata(&path_owned)
            .map_err(|e| format!("reading {label} {path_owned}: {e}"))?;
        if !meta.is_file() {
            return Err(format!("{label} {path_owned} must be a regular file"));
        }
        let file = std::fs::File::open(&path_owned)
            .map_err(|e| format!("reading {label} {path_owned}: {e}"))?;
        let fmeta = file
            .metadata()
            .map_err(|e| format!("reading {label} {path_owned}: {e}"))?;
        if !fmeta.is_file() {
            return Err(format!("{label} {path_owned} must be a regular file"));
        }
        if fmeta.len() > max_bytes {
            return Err(format!(
                "{label} {path_owned} exceeds {max_bytes} byte limit"
            ));
        }
        let mut buf = Vec::new();
        file.take(max_bytes.saturating_add(1))
            .read_to_end(&mut buf)
            .map_err(|e| format!("reading {label} {path_owned}: {e}"))?;
        if buf.len() as u64 > max_bytes {
            return Err(format!(
                "{label} {path_owned} exceeds {max_bytes} byte limit"
            ));
        }
        String::from_utf8(buf)
            .map_err(|e| format!("reading {label} {path_owned}: invalid UTF-8: {e}"))
    })
    .await
    .map_err(|e| format!("reading {label} {path}: task failed: {e}"))?
}

const MAX_PROVIDERS: usize = 64;

fn resolve_providers(args: &Value) -> Result<Vec<ProviderSpec>, String> {
    let names = args
        .get("providers")
        .and_then(Value::as_array)
        .ok_or("'providers' must be an array of provider ids")?;
    if names.is_empty() {
        return Err("'providers' must not be empty".to_string());
    }
    if names.len() > MAX_PROVIDERS {
        return Err(format!(
            "'providers' exceeds maximum of {MAX_PROVIDERS} entries"
        ));
    }
    let mut specs = Vec::with_capacity(names.len());
    for name in names {
        let name = name
            .as_str()
            .ok_or("each provider must be a string".to_string())?;
        specs.push(
            ProviderSpec::parse(name)
                .ok_or_else(|| format!("unknown provider '{name}' (try llmrace_list)"))?,
        );
    }
    Ok(specs)
}

async fn build_race(args: &Value) -> Result<Race, String> {
    let specs = resolve_providers(args)?;
    let prompt = opt_str(args, "prompt")?;
    let workload_path = opt_str(args, "workload")?;
    let runs = opt_usize_in_range(args, "runs", 1, 1000)?;
    let warmup = opt_usize_in_range(args, "warmup", 0, 100)?;
    let max_tokens = opt_u64_in_range(args, "max_tokens", 1, 131_072)?;
    let concurrency = opt_usize_in_range(args, "concurrency", 1, 128)?;
    let retries = opt_usize_in_range(args, "retries", 0, 10)?;
    let timeout_secs = opt_u64_in_range(args, "timeout_secs", 1, 600)?;
    let warmup_mode = match opt_str(args, "warmup_mode")? {
        None => None,
        Some("same-prompt") => Some(WarmupMode::SamePrompt),
        Some("probe") => Some(WarmupMode::ConnectionProbe),
        Some(other) => {
            return Err(format!(
                "invalid warmup_mode '{other}' (expected 'same-prompt' or 'probe')"
            ))
        }
    };
    let reproducible = opt_bool(args, "reproducible")?;
    let system = opt_str(args, "system")?.map(str::to_string);
    let tools = opt_bool(args, "tools")?;
    let json_output = opt_bool(args, "json_output")?;
    let response_schema = opt_object(args, "response_schema")?;
    if json_output == Some(true) && response_schema.is_some() {
        return Err("'json_output' and 'response_schema' cannot both be set".into());
    }
    let winner = opt_bool(args, "winner")?;

    let mut race = Race::new(specs).prompt(DEFAULT_PROMPT);
    if tools == Some(true) {
        race = race.tools("weather");
    }
    if let Some(prompt) = prompt {
        race = race.prompt(prompt);
    }
    if let Some(path) = workload_path {
        let raw = read_regular_file_bounded(path, "workload").await?;
        let workload =
            llmrace::workload::parse(&raw).map_err(|e| format!("loading workload {path}: {e}"))?;
        race = race.workload(workload);
    }
    if let Some(runs) = runs {
        race = race.runs(runs);
    }
    if let Some(warmup) = warmup {
        race = race.warmup(warmup);
    }
    if let Some(warmup_mode) = warmup_mode {
        race = race.warmup_mode(warmup_mode);
    }
    if let Some(max_tokens) = max_tokens {
        race = race.max_tokens(max_tokens);
    }
    if let Some(concurrency) = concurrency {
        race = race.concurrency(concurrency);
    }
    if let Some(retries) = retries {
        race = race.retries(retries);
    }
    if let Some(timeout_secs) = timeout_secs {
        race = race.timeout(timeout_secs);
    }
    if system.is_some() || json_output == Some(true) || response_schema.is_some() {
        let json_output = match response_schema {
            Some(schema) => Some(JsonOutput::Schema(schema)),
            None => (json_output == Some(true)).then_some(JsonOutput::Object),
        };
        let sampling = Sampling {
            system,
            json_output,
            ..Sampling::default()
        }
        .checked()?;
        race = race.sampling(sampling);
    }
    if reproducible == Some(true) {
        race = race.reproducible();
    }
    if winner == Some(true) {
        race = race.winner(llmrace::score::Weights::default());
    }
    Ok(race)
}

async fn tool_race(args: &Value) -> Result<String, String> {
    let outcome = build_race(args)
        .await?
        .run()
        .await
        .map_err(|e| e.to_string())?;
    if llmrace::gates::all_lanes_failed(&outcome.summaries) {
        Err(outcome.markdown())
    } else {
        Ok(outcome.markdown())
    }
}

async fn tool_gate(args: &Value) -> Result<String, String> {
    let baseline_path = opt_str(args, "baseline")?
        .ok_or_else(|| "'baseline' (path to a race.json report) is required".to_string())?;
    let threshold = opt_f64_non_neg(args, "max_regression")?.unwrap_or(15.0);
    let require_significance = opt_bool(args, "require_significance")?.unwrap_or(true);
    let fail_on_inconclusive = opt_bool(args, "fail_on_inconclusive")?.unwrap_or(false);
    let raw = read_regular_file_bounded(baseline_path, "baseline").await?;
    let baseline = BaselineReport::from_json_str(&raw)
        .map_err(|e| format!("parsing baseline {baseline_path}: {e}"))?;
    let outcome = build_race(args)
        .await?
        .run()
        .await
        .map_err(|e| e.to_string())?;
    let policy = GatePolicy {
        max_regression_pct: threshold,
        require_significance,
        fail_on_inconclusive,
    };
    let gate = outcome.gate(&baseline, policy);
    let failed = policy.failed(&gate);
    let body = serde_json::json!({
        "ok": !failed,
        "verdict": gate.verdict_json_with_policy(
            policy.max_regression_pct,
            policy.require_significance,
            policy.fail_on_inconclusive,
        ),
    });
    let mut text = serde_json::to_string_pretty(&body).map_err(|e| e.to_string())?;
    text.push_str(&gate.markdown_section(policy.max_regression_pct, policy.require_significance));
    if policy.fail_on_inconclusive && gate.inconclusive(policy.require_significance) {
        text.push_str("\nThe gate fails because fail_on_inconclusive requires usable evidence.\n");
    }
    if failed {
        Err(text)
    } else {
        Ok(text)
    }
}

fn tool_doctor() -> Result<String, String> {
    let checks: Vec<Value> = ProviderSpec::registry()
        .iter()
        .map(|p| {
            json!({
                "id": p.id.as_ref(),
                "default_model": p.default_model.as_ref(),
                "available": p.is_available(),
                "experimental": p.experimental,
                "opt_in": p.opt_in,
            })
        })
        .collect();
    serde_json::to_string_pretty(&checks).map_err(|e| e.to_string())
}

fn tool_list() -> Result<String, String> {
    let providers: Vec<Value> = ProviderSpec::registry()
        .iter()
        .map(|p| {
            json!({
                "id": p.id.as_ref(),
                "default_model": p.default_model.as_ref(),
                "wire": format!("{:?}", p.wire),
                "experimental": p.experimental,
                "opt_in": p.opt_in,
                "api_key_env": p.api_key_env.as_deref(),
            })
        })
        .collect();
    serde_json::to_string_pretty(&providers).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bounded_line_reader_handles_oversized_frames_and_recovers() {
        let mut input = Vec::new();
        input.extend_from_slice(b"first\r\n");
        input.extend_from_slice(&[b'x'; 20]);
        input.push(b'\n');
        input.extend_from_slice(b"after\n");
        input.extend_from_slice(&[0xff, 0xfe, b'\n']);
        input.extend_from_slice(b"tail");

        let mut reader = BufReader::with_capacity(4, input.as_slice());
        assert!(matches!(
            read_bounded_line(&mut reader, 10).await.unwrap(),
            LineRead::Line(ref s) if s == "first"
        ));
        assert!(matches!(
            read_bounded_line(&mut reader, 10).await.unwrap(),
            LineRead::TooLong
        ));
        assert!(matches!(
            read_bounded_line(&mut reader, 10).await.unwrap(),
            LineRead::Line(ref s) if s == "after"
        ));
        assert!(matches!(
            read_bounded_line(&mut reader, 10).await.unwrap(),
            LineRead::InvalidUtf8
        ));
        assert!(matches!(
            read_bounded_line(&mut reader, 10).await.unwrap(),
            LineRead::Line(ref s) if s == "tail"
        ));
        assert!(matches!(
            read_bounded_line(&mut reader, 10).await.unwrap(),
            LineRead::Eof
        ));
    }

    #[test]
    fn parameter_extractors_reject_wrong_types_and_out_of_range() {
        assert!(opt_usize_in_range(&json!({"runs": -1}), "runs", 1, 1000).is_err());
        assert!(opt_usize_in_range(&json!({"runs": "abc"}), "runs", 1, 1000).is_err());
        assert!(opt_usize_in_range(&json!({"runs": 0}), "runs", 1, 1000).is_err());
        assert!(opt_usize_in_range(&json!({"runs": 10_000_000}), "runs", 1, 1000).is_err());
        assert!(opt_usize_in_range(&json!({"runs": null}), "runs", 1, 1000).is_err());
        assert_eq!(
            opt_usize_in_range(&json!({"runs": 1000}), "runs", 1, 1000).unwrap(),
            Some(1000)
        );
        assert_eq!(
            opt_usize_in_range(&json!({}), "runs", 1, 1000).unwrap(),
            None
        );

        assert!(opt_u64_in_range(&json!({"max_tokens": 0}), "max_tokens", 1, 131_072).is_err());
        assert!(
            opt_u64_in_range(&json!({"max_tokens": 200_000}), "max_tokens", 1, 131_072).is_err()
        );
        assert_eq!(
            opt_u64_in_range(&json!({"max_tokens": 131_072}), "max_tokens", 1, 131_072).unwrap(),
            Some(131_072)
        );

        assert!(opt_f64_non_neg(&json!({"max_regression": "bad"}), "max_regression").is_err());
        assert!(opt_f64_non_neg(&json!({"max_regression": -0.1}), "max_regression").is_err());
        assert_eq!(
            opt_f64_non_neg(&json!({"max_regression": 0}), "max_regression").unwrap(),
            Some(0.0)
        );

        assert!(opt_bool(&json!({"tools": "yes"}), "tools").is_err());
        assert_eq!(
            opt_bool(&json!({"tools": true}), "tools").unwrap(),
            Some(true)
        );

        assert!(opt_str(&json!({"system": 42}), "system").is_err());
        assert_eq!(
            opt_str(&json!({"system": "ok"}), "system").unwrap(),
            Some("ok")
        );
    }

    #[tokio::test]
    async fn bounded_file_reader_rejects_directories_and_oversized_files() {
        let dir = std::env::temp_dir().join(format!("llmrace-mcp-unit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let dir_err = read_regular_file_with_limit(dir.to_str().unwrap(), "baseline", 64)
            .await
            .unwrap_err();
        assert!(dir_err.contains("regular file"), "{dir_err}");

        let file = dir.join("sample.txt");
        std::fs::write(&file, "a".repeat(100)).unwrap();
        let big_err = read_regular_file_with_limit(file.to_str().unwrap(), "baseline", 64)
            .await
            .unwrap_err();
        assert!(big_err.contains("exceeds 64 byte limit"), "{big_err}");

        let ok = read_regular_file_with_limit(file.to_str().unwrap(), "baseline", 100)
            .await
            .unwrap();
        assert_eq!(ok.len(), 100);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn build_race_wires_all_parity_options_and_validates_warmup_mode() {
        let bad = build_race(&json!({
            "providers": ["mock"],
            "warmup_mode": "invalid"
        }))
        .await;
        assert!(bad.is_err());

        let race = build_race(&json!({
            "providers": ["mock"],
            "runs": 1,
            "warmup": 1,
            "warmup_mode": "probe",
            "max_tokens": 64,
            "concurrency": 2,
            "retries": 1,
            "timeout_secs": 30,
            "reproducible": true,
            "system": "Be concise.",
            "tools": true,
            "json_output": true
        }))
        .await
        .unwrap();
        let outcome = race.run().await.unwrap();
        let report: Value = serde_json::from_str(&outcome.json()).unwrap();
        assert_eq!(report["experiment"]["warmup_mode"], "probe");
        assert_eq!(report["experiment"]["concurrency"], 2);
        assert_eq!(report["experiment"]["retries"], 1);
        assert_eq!(report["experiment"]["timeout_secs"], 30);
        assert_eq!(report["sampling"]["temperature"], 0.0);
        assert_eq!(report["sampling"]["top_p"], 1.0);
        assert_eq!(report["sampling"]["seed"], 0);
        assert_eq!(report["sampling"]["system"], "Be concise.");
        assert_eq!(report["sampling"]["json_output"], "object");
    }

    #[test]
    fn resolve_providers_caps_array_length() {
        let too_many: Vec<Value> = (0..=MAX_PROVIDERS).map(|_| json!("mock")).collect();
        let err = resolve_providers(&json!({"providers": too_many})).unwrap_err();
        assert!(err.contains("exceeds maximum"), "{err}");
    }

    #[tokio::test]
    async fn build_race_response_schema_stamps_schema_and_enforces_constraints() {
        let schema = json!({
            "type": "object",
            "properties": {"answer": {"type": "string"}},
            "required": ["answer"],
            "additionalProperties": false
        });

        // response_schema alone stamps report["sampling"]["json_output"] == "schema"
        let race = build_race(&json!({
            "providers": ["mock"],
            "runs": 1,
            "warmup": 0,
            "response_schema": schema
        }))
        .await
        .unwrap();
        let outcome = race.run().await.unwrap();
        let report: Value = serde_json::from_str(&outcome.json()).unwrap();
        assert_eq!(report["sampling"]["json_output"], "schema");

        // json_output: false + response_schema is accepted cleanly
        let race_false = build_race(&json!({
            "providers": ["mock"],
            "runs": 1,
            "warmup": 0,
            "json_output": false,
            "response_schema": schema
        }))
        .await
        .unwrap();
        let outcome_false = race_false.run().await.unwrap();
        let report_false: Value = serde_json::from_str(&outcome_false.json()).unwrap();
        assert_eq!(report_false["sampling"]["json_output"], "schema");

        // json_output: true + response_schema is rejected
        let both_err = build_race(&json!({
            "providers": ["mock"],
            "json_output": true,
            "response_schema": schema
        }))
        .await
        .err()
        .unwrap();
        assert!(
            both_err.contains("json_output") && both_err.contains("response_schema"),
            "{both_err}"
        );

        // Non-object response_schema (filesystem path string or array) is rejected
        for invalid in [
            json!("/etc/passwd"),
            json!(["not", "an", "object"]),
            json!(null),
        ] {
            let err = build_race(&json!({
                "providers": ["mock"],
                "response_schema": invalid
            }))
            .await
            .err()
            .unwrap();
            assert!(err.contains("must be a JSON object"), "{err}");
        }

        // Oversized (> 256 KiB) inline response_schema is rejected via Sampling::checked()
        let huge = json!({
            "type": "object",
            "description": "x".repeat(256 * 1024 + 64)
        });
        let oversized_err = build_race(&json!({
            "providers": ["mock"],
            "response_schema": huge
        }))
        .await
        .err()
        .unwrap();
        assert!(
            oversized_err.contains("256")
                || oversized_err.contains("262144")
                || oversized_err.to_ascii_lowercase().contains("schema"),
            "{oversized_err}"
        );

        // Tool schemas expose response_schema as an inline JSON object
        let list = tools_list();
        let tools = list["tools"].as_array().unwrap();
        for tool_name in ["llmrace_race", "llmrace_gate"] {
            let tool = tools
                .iter()
                .find(|t| t["name"] == tool_name)
                .expect("tool listed");
            assert_eq!(
                tool["inputSchema"]["properties"]["response_schema"]["type"],
                "object"
            );
        }
    }
}
