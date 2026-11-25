//! MCP server: llmrace as a tool set for any MCP-capable agent.
//!
//! `llmrace --mcp` speaks newline-delimited JSON-RPC 2.0 on stdio. Four
//! tools: `llmrace_race`, `llmrace_gate`, `llmrace_doctor`, `llmrace_list`.
//! API keys come from the environment; values never appear in responses.

use anyhow::Result;
use llmrace::provider::ProviderSpec;
use llmrace::race::{GatePolicy, Race};
use llmrace::regression::BaselineReport;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const PROTOCOL_VERSION: &str = "2024-11-05";

/// Runs the server until stdin closes.
pub(crate) async fn serve() -> Result<()> {
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut out = tokio::io::stdout();
    while let Some(line) = lines.next_line().await? {
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(&line) {
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
            "inputSchema": {
                "type": "object",
                "required": ["providers", "baseline"],
                "properties": {
                    "providers": {"type": "array", "items": {"type": "string"}},
                    "baseline": {"type": "string", "description": "Path to a race.json baseline report"},
                    "prompt": {"type": "string"},
                    "workload": {"type": "string", "description": "Path to a JSONL workload file"},
                    "runs": {"type": "integer", "minimum": 1},
                    "warmup": {"type": "integer", "minimum": 0},
                    "max_tokens": {"type": "integer", "minimum": 1},
                    "max_regression": {"type": "number", "description": "Threshold in percent (default 15)"},
                    "require_significance": {"type": "boolean", "description": "Latency/throughput breaches must be significant (default true)"}
                }
            },
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

fn race_schema() -> Value {
    json!({
        "type": "object",
        "required": ["providers"],
        "properties": {
            "providers": {"type": "array", "items": {"type": "string"}},
            "prompt": {"type": "string"},
            "workload": {"type": "string", "description": "Path to a JSONL workload file"},
            "runs": {"type": "integer", "minimum": 1},
            "warmup": {"type": "integer", "minimum": 0},
            "max_tokens": {"type": "integer", "minimum": 1},
            "winner": {"type": "boolean", "description": "Include the weighted podium"}
        }
    })
}

async fn call_tool(id: Value, msg: &Value) -> Value {
    let params = &msg["params"];
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
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

fn resolve_providers(args: &Value) -> Result<Vec<ProviderSpec>, String> {
    let names = args
        .get("providers")
        .and_then(Value::as_array)
        .ok_or("'providers' must be an array of provider ids")?;
    if names.is_empty() {
        return Err("'providers' must not be empty".to_string());
    }
    let mut specs = Vec::with_capacity(names.len());
    for name in names {
        let name = name
            .as_str()
            .ok_or("each provider must be a string".to_string())?;
        let (id, model) = match name.split_once('=') {
            Some((id, m)) if !id.is_empty() && !m.is_empty() => (id, Some(m)),
            _ => (name, None),
        };
        let mut spec = ProviderSpec::find(id)
            .ok_or_else(|| format!("unknown provider '{id}' (try llmrace_list)"))?;
        if let Some(model) = model {
            spec.default_model = std::sync::Arc::from(model);
        }
        specs.push(spec);
    }
    Ok(specs)
}

/// The prompt the CLI sends when none is given, so an MCP race and a CLI
/// race hash the same request by default.
const DEFAULT_PROMPT: &str = "Write one vivid paragraph about a lighthouse at dawn.";

async fn build_race(args: &Value) -> Result<Race, String> {
    let specs = resolve_providers(args)?;
    let mut race = Race::new(specs);
    if let Some(prompt) = args.get("prompt").and_then(Value::as_str) {
        race = race.prompt(prompt);
    } else if args.get("workload").is_none() {
        race = race.prompt(DEFAULT_PROMPT);
    }
    if let Some(path) = args.get("workload").and_then(Value::as_str) {
        let workload = llmrace::workload::load(std::path::Path::new(path))
            .map_err(|e| format!("loading workload {path}: {e}"))?;
        race = race.workload(workload);
    }
    if let Some(runs) = args.get("runs").and_then(Value::as_u64) {
        race = race.runs(runs as usize);
    }
    if let Some(warmup) = args.get("warmup").and_then(Value::as_u64) {
        race = race.warmup(warmup as usize);
    }
    if let Some(max_tokens) = args.get("max_tokens").and_then(Value::as_u64) {
        race = race.max_tokens(max_tokens);
    }
    if args.get("winner").and_then(Value::as_bool) == Some(true) {
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
    Ok(outcome.markdown())
}

async fn tool_gate(args: &Value) -> Result<String, String> {
    let baseline_path = args
        .get("baseline")
        .and_then(Value::as_str)
        .ok_or("'baseline' (path to a race.json report) is required".to_string())?;
    let raw = std::fs::read_to_string(baseline_path)
        .map_err(|e| format!("reading baseline {baseline_path}: {e}"))?;
    let baseline = BaselineReport::from_json_str(&raw)
        .map_err(|e| format!("parsing baseline {baseline_path}: {e}"))?;
    let outcome = build_race(args)
        .await?
        .run()
        .await
        .map_err(|e| e.to_string())?;
    let policy = GatePolicy {
        max_regression_pct: args
            .get("max_regression")
            .and_then(Value::as_f64)
            .unwrap_or(15.0),
        require_significance: args
            .get("require_significance")
            .and_then(Value::as_bool)
            .unwrap_or(true),
    };
    let gate = outcome.gate(&baseline, policy);
    let failed = gate.gate_failed(policy.require_significance);
    let body = serde_json::json!({
        "ok": !failed,
        "verdict": gate.verdict_json(policy.max_regression_pct, policy.require_significance),
    });
    let mut text = serde_json::to_string_pretty(&body).map_err(|e| e.to_string())?;
    text.push_str(&gate.markdown_section(policy.max_regression_pct, policy.require_significance));
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
