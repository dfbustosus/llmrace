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
    let parameters = (task.parameters)();
    match wire {
        Wire::OpenAiCompat | Wire::Cohere => json!([{
            "type": "function",
            "function": {
                "name": task.function,
                "description": task.description,
                "parameters": parameters,
            }
        }]),
        Wire::Anthropic => json!([{
            "name": task.function,
            "description": task.description,
            "input_schema": parameters,
        }]),
        Wire::Gemini => json!([{
            "function_declarations": [{
                "name": task.function,
                "description": task.description,
                "parameters": parameters,
            }]
        }]),
        Wire::Bedrock | Wire::Vertex | Wire::Mock => json!([]),
    }
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
    task_ok_match(
        tools,
        tool_task,
        tool_args,
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
    let mut checks = Vec::new();
    if tools {
        let validate = find(tool_task)
            .map(|task| task.validate)
            .unwrap_or(validate_args);
        checks.push(!tool_args.is_empty() && validate(tool_args));
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
