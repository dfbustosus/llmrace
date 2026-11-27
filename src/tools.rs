//! Tool-call benchmarking: a registry of tool tasks, per-wire tool
//! schemas, and per-task argument validation.
//!
//! Tasks are selected with `--tools [name]` (default `weather`).

use crate::provider::Wire;
use serde_json::{json, Value};

/// One benchmarkable tool-calling task.
#[derive(Debug, Clone, Copy)]
pub struct ToolTask {
    /// Registry name used on the CLI (`--tools <name>`).
    pub name: &'static str,
    /// One-line description for `--help`-style listings.
    pub description: &'static str,
    /// Function name sent in the tool schema.
    pub function: &'static str,
    /// User prompt that should trigger the tool call.
    pub prompt: &'static str,
    /// JSON-schema `parameters` object for the function.
    pub parameters: fn() -> Value,
    /// Validates accumulated tool-call argument JSON for this task.
    pub validate: fn(&str) -> bool,
}

/// The weather task: `get_weather(city)`.
fn weather() -> ToolTask {
    ToolTask {
        name: "weather",
        description: "get_weather(city) — classic single-argument lookup",
        function: "get_weather",
        prompt: "What is the weather in Paris right now? Use the provided tool.",
        parameters: || {
            json!({
                "type": "object",
                "properties": {
                    "city": {"type": "string", "description": "City name"},
                    "units": {"type": "string", "enum": ["celsius", "fahrenheit"]}
                },
                "required": ["city"]
            })
        },
        validate: |args| {
            serde_json::from_str::<Value>(args)
                .ok()
                .and_then(|v| v.get("city").and_then(Value::as_str).map(str::to_string))
                .is_some_and(|c| !c.is_empty())
        },
    }
}

/// The math task: `calculate(expression)`.
fn math() -> ToolTask {
    ToolTask {
        name: "math",
        description: "calculate(expression) — argument must contain a digit",
        function: "calculate",
        prompt: "What is 17 * 23? Use the calculator tool.",
        parameters: || {
            json!({
                "type": "object",
                "properties": {
                    "expression": {"type": "string", "description": "Arithmetic expression"}
                },
                "required": ["expression"]
            })
        },
        validate: |args| {
            serde_json::from_str::<Value>(args)
                .ok()
                .and_then(|v| {
                    v.get("expression")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .is_some_and(|e| e.chars().any(|c| c.is_ascii_digit()))
        },
    }
}

/// The lookup task: `search_docs(query)`.
fn lookup() -> ToolTask {
    ToolTask {
        name: "lookup",
        description: "search_docs(query) — open-ended retrieval query",
        function: "search_docs",
        prompt: "When was Rust 1.0 released? Use the search tool.",
        parameters: || {
            json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Search query"}
                },
                "required": ["query"]
            })
        },
        validate: |args| {
            serde_json::from_str::<Value>(args)
                .ok()
                .and_then(|v| v.get("query").and_then(Value::as_str).map(str::to_string))
                .is_some_and(|q| !q.is_empty())
        },
    }
}

/// A tool declared on a workload prompt.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ToolDefinition {
    /// Function name sent in the tool schema.
    pub name: String,
    /// One-line description sent with the schema.
    #[serde(default)]
    pub description: String,
    /// JSON Schema for the arguments object.
    pub parameters: serde_json::Value,
}

/// All built-in tool tasks.
pub fn registry() -> Vec<ToolTask> {
    vec![weather(), math(), lookup()]
}

/// Looks up a task by name; `None` for unknown names.
#[must_use]
pub fn find(name: &str) -> Option<ToolTask> {
    registry().into_iter().find(|t| t.name == name)
}

/// Builds the wire-specific `tools` request field for a task.
#[must_use]
pub fn tool_schema(task: &ToolTask, wire: Wire) -> Value {
    tool_body(
        wire,
        &ToolDefinition {
            name: task.function.to_string(),
            description: task.description.to_string(),
            parameters: (task.parameters)(),
        },
    )
}

/// The JSON value assigned to a wire's tools field for one definition.
///
/// Bedrock receives a `toolConfig` document. Vertex uses the Gemini
/// `function_declarations` shape. Mock receives nothing.
#[must_use]
pub fn tool_body(wire: Wire, tool: &ToolDefinition) -> Value {
    match wire {
        Wire::OpenAiCompat | Wire::Cohere => json!([{
            "type": "function",
            "function": {
                "name": tool.name,
                "description": tool.description,
                "parameters": tool.parameters,
            }
        }]),
        Wire::OpenAiResponses => json!([{
            "type": "function",
            "name": tool.name,
            "description": tool.description,
            "parameters": tool.parameters,
        }]),
        Wire::Anthropic => json!([{
            "name": tool.name,
            "description": tool.description,
            "input_schema": tool.parameters,
        }]),
        Wire::Gemini | Wire::Vertex => json!([{
            "function_declarations": [{
                "name": tool.name,
                "description": tool.description,
                "parameters": tool.parameters,
            }]
        }]),
        Wire::Bedrock => json!({
            "tools": [{
                "toolSpec": {
                    "name": tool.name,
                    "description": tool.description,
                    "inputSchema": { "json": tool.parameters },
                }
            }]
        }),
        Wire::Mock => json!([]),
    }
}

/// Rejects a schema this crate will not evaluate.
///
/// The supported validation keywords are `type`, `required`, `properties`,
/// `additionalProperties`, `items`, `enum`, `minimum`, `maximum`,
/// `minLength` and `maxLength`. Standard annotation/metadata keywords
/// (`description`, `title`, `$schema`, `$comment`, `default`) are accepted
/// as no-ops so Pydantic, Zod and OpenAI tool schemas pass cleanly.
/// Nesting stops at 8 levels.
pub fn check_schema(schema: &Value) -> Result<(), String> {
    check_schema_at(schema, 0)
}

fn check_schema_at(schema: &Value, depth: usize) -> Result<(), String> {
    if depth > 8 {
        return Err("schema nests more than 8 levels".into());
    }
    let Some(object) = schema.as_object() else {
        return Err("schema must be a JSON object".into());
    };
    for key in object.keys() {
        match key.as_str() {
            "type"
            | "required"
            | "properties"
            | "additionalProperties"
            | "items"
            | "enum"
            | "minimum"
            | "maximum"
            | "minLength"
            | "maxLength"
            | "description"
            | "title"
            | "$schema"
            | "$comment"
            | "default" => {}
            other => return Err(format!("unsupported schema keyword '{other}'")),
        }
    }
    if let Some(kind) = object.get("type") {
        let ok = matches!(
            kind.as_str(),
            Some("object" | "array" | "string" | "number" | "integer" | "boolean" | "null")
        );
        if !ok {
            return Err(format!("unsupported schema type {kind}"));
        }
    }
    if let Some(required) = object.get("required") {
        let keys = required
            .as_array()
            .is_some_and(|items| items.iter().all(|k| k.is_string()));
        if !keys {
            return Err("required must be an array of strings".into());
        }
    }
    if let Some(properties) = object.get("properties") {
        let Some(properties) = properties.as_object() else {
            return Err("properties must be an object".into());
        };
        for schema in properties.values() {
            check_schema_at(schema, depth + 1)?;
        }
    }
    if let Some(extra) = object.get("additionalProperties") {
        if extra.as_bool().is_none() {
            check_schema_at(extra, depth + 1)?;
        }
    }
    if let Some(items) = object.get("items") {
        check_schema_at(items, depth + 1)?;
    }
    if let Some(enum_vals) = object.get("enum") {
        if !enum_vals.is_array() {
            return Err("enum must be an array".into());
        }
    }
    for key in ["minimum", "maximum"] {
        if object.get(key).is_some_and(|v| !v.is_number()) {
            return Err(format!("{key} must be a number"));
        }
    }
    for key in ["minLength", "maxLength"] {
        if object.get(key).is_some_and(|v| v.as_u64().is_none()) {
            return Err(format!("{key} must be an integer"));
        }
    }
    Ok(())
}

/// Whether `value` satisfies a schema accepted by [`check_schema`].
#[must_use]
pub fn value_matches_schema(value: &Value, schema: &Value) -> bool {
    let Some(object) = schema.as_object() else {
        return false;
    };
    if let Some(enum_vals) = object.get("enum").and_then(Value::as_array) {
        if !enum_vals.iter().any(|item| item == value) {
            return false;
        }
    }
    if let Some(kind) = object.get("type").and_then(Value::as_str) {
        let typed = match kind {
            "object" => value.is_object(),
            "array" => value.is_array(),
            "string" => value.is_string(),
            "number" => value.is_number(),
            "integer" => {
                value.as_i64().is_some()
                    || value.as_u64().is_some()
                    || value
                        .as_f64()
                        .is_some_and(|f| f.is_finite() && f.fract() == 0.0)
            }
            "boolean" => value.is_boolean(),
            "null" => value.is_null(),
            _ => false,
        };
        if !typed {
            return false;
        }
    }
    match value {
        Value::Object(map) => {
            if let Some(required) = object.get("required").and_then(Value::as_array) {
                for key in required {
                    let Some(key) = key.as_str() else {
                        return false;
                    };
                    if !map.contains_key(key) {
                        return false;
                    }
                }
            }
            if let Some(properties) = object.get("properties").and_then(Value::as_object) {
                for (key, sub) in properties {
                    if let Some(child) = map.get(key) {
                        if !value_matches_schema(child, sub) {
                            return false;
                        }
                    }
                }
            }
            match object.get("additionalProperties") {
                Some(Value::Bool(false)) => {
                    let known = object.get("properties").and_then(Value::as_object);
                    if map
                        .keys()
                        .any(|key| known.is_none_or(|props| !props.contains_key(key)))
                    {
                        return false;
                    }
                }
                Some(sub) if sub.is_object() => {
                    let known = object.get("properties").and_then(Value::as_object);
                    for (key, child) in map {
                        if known.is_none_or(|props| !props.contains_key(key))
                            && !value_matches_schema(child, sub)
                        {
                            return false;
                        }
                    }
                }
                _ => {}
            }
        }
        Value::Array(items) => {
            if let Some(item_schema) = object.get("items") {
                if items
                    .iter()
                    .any(|item| !value_matches_schema(item, item_schema))
                {
                    return false;
                }
            }
        }
        Value::String(text) => {
            let len = text.chars().count() as u64;
            if let Some(min) = object.get("minLength").and_then(Value::as_u64) {
                if len < min {
                    return false;
                }
            }
            if let Some(max) = object.get("maxLength").and_then(Value::as_u64) {
                if len > max {
                    return false;
                }
            }
        }
        Value::Number(number) => {
            let Some(number) = number.as_f64() else {
                return false;
            };
            if let Some(min) = object.get("minimum").and_then(Value::as_f64) {
                if number < min {
                    return false;
                }
            }
            if let Some(max) = object.get("maximum").and_then(Value::as_f64) {
                if number > max {
                    return false;
                }
            }
        }
        _ => {}
    }
    true
}

/// Validates accumulated tool-call arguments as JSON containing a `city`.
///
/// Legacy helper for the default weather task; prefer [`ToolTask::validate`]
/// via [`find`] when the task is selectable.
pub fn validate_args(args: &str) -> bool {
    (weather().validate)(args)
}

/// Exact match for a short answer: the trimmed text, a JSON object
/// (key order does not matter), or a JSON number.
#[must_use]
pub fn answer_matches(answer: &str, expect: &str) -> bool {
    let answer = answer.trim();
    let expect = expect.trim();
    if answer == expect {
        return true;
    }
    let Ok(got) = serde_json::from_str::<Value>(answer) else {
        return false;
    };
    let Ok(want) = serde_json::from_str::<Value>(expect) else {
        return false;
    };
    (got.is_object() && want.is_object() || got.is_number() && want.is_number()) && got == want
}

/// Compiles an answer regex with a 1 MiB size limit.
///
/// Called when the workload is read, so an invalid or oversized pattern
/// fails before any request is sent.
pub fn compile_answer_regex(pattern: &str) -> Result<regex::Regex, regex::Error> {
    regex::RegexBuilder::new(pattern)
        .size_limit(1 << 20)
        .dfa_size_limit(1 << 20)
        .build()
}

/// Compares a trimmed answer using the workload's `match` mode.
///
/// [`crate::workload::ExpectMatch::Exact`] is [`answer_matches`]. `contains`
/// is a case-sensitive substring. `regex` uses the pattern as written.
#[must_use]
pub fn answer_matches_mode(answer: &str, expect: &str, mode: crate::workload::ExpectMatch) -> bool {
    use crate::workload::ExpectMatch;
    match mode {
        ExpectMatch::Exact => answer_matches(answer, expect),
        ExpectMatch::Contains => answer.trim().contains(expect),
        ExpectMatch::Regex => {
            compile_answer_regex(expect).is_ok_and(|re| re.is_match(answer.trim()))
        }
        ExpectMatch::JsonSchema => serde_json::from_str::<Value>(answer.trim())
            .ok()
            .zip(serde_json::from_str::<Value>(expect).ok())
            .is_some_and(|(value, schema)| value_matches_schema(&value, &schema)),
    }
}

/// Whether this successful response passed the task.
///
/// `None` when the round had no task. Tool validity and an `expect`
/// check both apply when both are set; either failure fails the task. The
/// HTTP call can still have succeeded. `expect` is an exact check; use
/// [`task_ok_match`] for `contains` and `regex`.
#[must_use]
pub fn task_ok(
    tools: bool,
    tool_task: &str,
    tool_args: &str,
    expect: Option<&str>,
    answer: &str,
) -> Option<bool> {
    task_ok_with_tool(
        tools,
        tool_task,
        tool_args,
        None,
        expect,
        crate::workload::ExpectMatch::Exact,
        answer,
    )
}

/// Like [`task_ok`], with the workload's `match` mode.
#[must_use]
pub fn task_ok_match(
    tools: bool,
    tool_task: &str,
    tool_args: &str,
    expect: Option<&str>,
    mode: crate::workload::ExpectMatch,
    answer: &str,
) -> Option<bool> {
    task_ok_with_tool(tools, tool_task, tool_args, None, expect, mode, answer)
}

/// Like [`task_ok_match`], and checks a workload tool schema when one was sent.
///
/// `tool_parameters` replaces the built-in task validator. `None` keeps the
/// registry task named by `tool_task`.
#[must_use]
pub fn task_ok_with_tool(
    tools: bool,
    tool_task: &str,
    tool_args: &str,
    tool_parameters: Option<&Value>,
    expect: Option<&str>,
    mode: crate::workload::ExpectMatch,
    answer: &str,
) -> Option<bool> {
    let mut checks = Vec::new();
    if tools {
        let valid = match tool_parameters {
            Some(schema) => serde_json::from_str::<Value>(tool_args)
                .ok()
                .is_some_and(|value| value_matches_schema(&value, schema)),
            None => {
                let validate = find(tool_task)
                    .map(|task| task.validate)
                    .unwrap_or(validate_args);
                !tool_args.is_empty() && validate(tool_args)
            }
        };
        checks.push(valid);
    }
    if let Some(expect) = expect {
        checks.push(answer_matches_mode(answer, expect, mode));
    }
    if checks.is_empty() {
        None
    } else {
        Some(checks.into_iter().all(|ok| ok))
    }
}

/// The user prompt for the default (weather) tool-calling task.
pub fn tool_task_prompt() -> &'static str {
    weather().prompt
}

/// Builds the wire-specific `tools` request field for the weather task.
///
/// Legacy helper; prefer [`tool_schema`] with a task from [`find`].
pub fn weather_tool(wire: Wire) -> Value {
    tool_schema(&weather(), wire)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_tool_schemas_and_annotations_pass_check_schema() {
        for task in registry() {
            let params = (task.parameters)();
            assert!(
                check_schema(&params).is_ok(),
                "{} failed check_schema: {:?}",
                task.name,
                check_schema(&params)
            );
        }
        let pydantic_style = json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "title": "WeatherQuery",
            "description": "Lookup query",
            "$comment": "generated",
            "type": "object",
            "properties": {
                "city": {
                    "type": "string",
                    "description": "City name",
                    "default": "Paris"
                }
            },
            "required": ["city"]
        });
        assert!(check_schema(&pydantic_style).is_ok());
    }

    #[test]
    fn integer_schema_accepts_whole_floats_and_rejects_fractions() {
        let schema = json!({"type": "integer"});
        assert!(value_matches_schema(&json!(5), &schema));
        assert!(value_matches_schema(&json!(5.0), &schema));
        assert!(value_matches_schema(&json!(-3.0), &schema));
        assert!(!value_matches_schema(&json!(5.5), &schema));
    }
}
