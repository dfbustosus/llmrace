//! Tool-call benchmarking: a registry of tool tasks, per-wire tool
//! schemas, and per-task argument validation.
//!
//! Tasks are selected with `--tools [name]` (default `weather`).

use crate::provider::Wire;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, OnceLock, RwLock};

const MAX_SCHEMA_DEPTH: usize = 8;
const MAX_MATCH_DEPTH: usize = 32;
const MAX_VALUE_DEPTH: usize = 64;
const MAX_ENUM_VALUE_DEPTH: usize = 16;
const MAX_SCHEMA_NODES: usize = 1024;
const MAX_SCHEMA_PATTERNS: usize = 32;
const MAX_ENUM_ITEMS: usize = 1024;
const MAX_REGEX_PATTERN_BYTES: usize = 4096;
const MAX_CACHED_REGEXES: usize = 64;

#[derive(Default)]
struct RegexCache {
    map: HashMap<String, Arc<regex::Regex>>,
    order: VecDeque<String>,
}

fn regex_cache() -> &'static RwLock<RegexCache> {
    static CACHE: OnceLock<RwLock<RegexCache>> = OnceLock::new();
    CACHE.get_or_init(|| RwLock::new(RegexCache::default()))
}

fn cached_answer_regex(pattern: &str) -> Result<Arc<regex::Regex>, regex::Error> {
    if pattern.len() > MAX_REGEX_PATTERN_BYTES {
        return compile_answer_regex(pattern).map(Arc::new);
    }
    let cache = regex_cache();
    {
        let guard = cache.read().unwrap_or_else(|e| e.into_inner());
        if let Some(re) = guard.map.get(pattern) {
            return Ok(Arc::clone(re));
        }
    }
    let compiled = Arc::new(compile_answer_regex(pattern)?);
    let mut guard = cache.write().unwrap_or_else(|e| e.into_inner());
    if let Some(re) = guard.map.get(pattern) {
        return Ok(Arc::clone(re));
    }
    while guard.map.len() >= MAX_CACHED_REGEXES {
        if let Some(oldest) = guard.order.pop_front() {
            guard.map.remove(&oldest);
        } else {
            break;
        }
    }
    guard.map.insert(pattern.to_string(), Arc::clone(&compiled));
    guard.order.push_back(pattern.to_string());
    Ok(compiled)
}

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

fn is_valid_schema_type(kind: &str) -> bool {
    matches!(
        kind,
        "object" | "array" | "string" | "number" | "integer" | "boolean" | "null"
    )
}

fn value_matches_type(value: &Value, kind: &str) -> bool {
    match kind {
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
    }
}

fn number_to_i128(n: &serde_json::Number) -> Option<i128> {
    n.as_i64()
        .map(i128::from)
        .or_else(|| n.as_u64().map(i128::from))
}

fn int_equals_float(int_num: &serde_json::Number, float_num: &serde_json::Number) -> bool {
    let (Some(i), Some(f)) = (number_to_i128(int_num), float_num.as_f64()) else {
        return false;
    };
    f.is_finite()
        && f.fract() == 0.0
        && (-9_223_372_036_854_775_808.0_f64..18_446_744_073_709_551_616.0_f64).contains(&f)
        && (f as i128) == i
}

fn json_values_equal(a: &Value, b: &Value) -> bool {
    json_values_equal_at(a, b, 0)
}

fn json_values_equal_at(a: &Value, b: &Value, depth: usize) -> bool {
    if depth > MAX_VALUE_DEPTH {
        return false;
    }
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::String(x), Value::String(y)) => x == y,
        (Value::Number(x), Value::Number(y)) => match (x.is_f64(), y.is_f64()) {
            (false, false) => matches!(
                (number_to_i128(x), number_to_i128(y)),
                (Some(xi), Some(yi)) if xi == yi
            ),
            (true, true) => x.as_f64() == y.as_f64(),
            (false, true) => int_equals_float(x, y),
            (true, false) => int_equals_float(y, x),
        },
        (Value::Array(xs), Value::Array(ys)) => {
            xs.len() == ys.len()
                && xs
                    .iter()
                    .zip(ys.iter())
                    .all(|(x, y)| json_values_equal_at(x, y, depth + 1))
        }
        (Value::Object(xs), Value::Object(ys)) => {
            xs.len() == ys.len()
                && xs.iter().all(|(k, xv)| {
                    ys.get(k)
                        .is_some_and(|yv| json_values_equal_at(xv, yv, depth + 1))
                })
        }
        _ => false,
    }
}

fn value_within_depth(value: &Value, depth: usize, max_depth: usize) -> bool {
    if depth > max_depth {
        return false;
    }
    match value {
        Value::Array(items) => items
            .iter()
            .all(|item| value_within_depth(item, depth + 1, max_depth)),
        Value::Object(map) => map
            .values()
            .all(|val| value_within_depth(val, depth + 1, max_depth)),
        _ => true,
    }
}

fn check_u64_bounds(
    object: &serde_json::Map<String, Value>,
    min_key: &str,
    max_key: &str,
) -> Result<(), String> {
    let min = optional_u64(object, min_key)
        .ok_or_else(|| format!("{min_key} must be a non-negative integer"))?;
    let max = optional_u64(object, max_key)
        .ok_or_else(|| format!("{max_key} must be a non-negative integer"))?;
    if let (Some(min), Some(max)) = (min, max) {
        if min > max {
            return Err(format!("{min_key} ({min}) exceeds {max_key} ({max})"));
        }
    }
    Ok(())
}

fn optional_u64(object: &serde_json::Map<String, Value>, key: &str) -> Option<Option<u64>> {
    match object.get(key) {
        Some(v) => v.as_u64().map(Some),
        None => Some(None),
    }
}

fn optional_finite_f64(object: &serde_json::Map<String, Value>, key: &str) -> Option<Option<f64>> {
    match object.get(key) {
        Some(v) => v.as_f64().filter(|n| n.is_finite()).map(Some),
        None => Some(None),
    }
}

/// Rejects a schema this crate will not evaluate.
///
/// The supported validation keywords are `type` (a primitive type string or a
/// non-empty array of unique primitive type strings), `required`, `properties`,
/// `additionalProperties`, `items`, `enum`, `const`, `pattern` (evaluated with
/// Rust `regex` syntax under a 1 MiB compiled/DFA size cap), `minimum`,
/// `maximum`, `minLength`, `maxLength`, `minItems`, and `maxItems`. Standard
/// annotation/metadata keywords (`description`, `title`, `$schema`, `$comment`,
/// `default`) are accepted as no-ops so Pydantic, Zod, and OpenAI tool schemas
/// pass cleanly. Nesting stops at 8 levels.
pub fn check_schema(schema: &Value) -> Result<(), String> {
    let mut nodes = 0;
    let mut patterns = 0;
    check_schema_at(schema, 0, &mut nodes, &mut patterns)
}

fn check_schema_at(
    schema: &Value,
    depth: usize,
    nodes: &mut usize,
    patterns: &mut usize,
) -> Result<(), String> {
    if depth > MAX_SCHEMA_DEPTH {
        return Err(format!("schema nests more than {MAX_SCHEMA_DEPTH} levels"));
    }
    *nodes += 1;
    if *nodes > MAX_SCHEMA_NODES {
        return Err(format!("schema exceeds {MAX_SCHEMA_NODES} nodes"));
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
            | "const"
            | "pattern"
            | "minimum"
            | "maximum"
            | "minLength"
            | "maxLength"
            | "minItems"
            | "maxItems"
            | "description"
            | "title"
            | "$schema"
            | "$comment"
            | "default" => {}
            other => return Err(format!("unsupported schema keyword '{other}'")),
        }
    }
    if let Some(kind) = object.get("type") {
        match kind {
            Value::String(s) if is_valid_schema_type(s) => {}
            Value::Array(items) => {
                if items.is_empty() || items.len() > 7 {
                    return Err("type array must contain 1 to 7 unique primitive types".into());
                }
                let mut seen = HashSet::with_capacity(items.len());
                for item in items {
                    let Some(s) = item.as_str() else {
                        return Err(format!("unsupported schema type {item}"));
                    };
                    if !is_valid_schema_type(s) {
                        return Err(format!("unsupported schema type {item}"));
                    }
                    if !seen.insert(s) {
                        return Err(format!("duplicate schema type '{s}'"));
                    }
                }
            }
            _ => return Err(format!("unsupported schema type {kind}")),
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
            check_schema_at(schema, depth + 1, nodes, patterns)?;
        }
    }
    if let Some(extra) = object.get("additionalProperties") {
        if extra.as_bool().is_none() {
            check_schema_at(extra, depth + 1, nodes, patterns)?;
        }
    }
    if let Some(items) = object.get("items") {
        check_schema_at(items, depth + 1, nodes, patterns)?;
    }
    if let Some(enum_vals) = object.get("enum") {
        let Some(items) = enum_vals.as_array() else {
            return Err("enum must be an array".into());
        };
        if items.is_empty() {
            return Err("enum must not be empty".into());
        }
        if items.len() > MAX_ENUM_ITEMS {
            return Err(format!("enum exceeds {MAX_ENUM_ITEMS} items"));
        }
        for item in items {
            if !value_within_depth(item, 0, MAX_ENUM_VALUE_DEPTH) {
                return Err(format!(
                    "enum value nests more than {MAX_ENUM_VALUE_DEPTH} levels"
                ));
            }
        }
    }
    if let Some(const_val) = object.get("const") {
        if !value_within_depth(const_val, 0, MAX_ENUM_VALUE_DEPTH) {
            return Err(format!(
                "const value nests more than {MAX_ENUM_VALUE_DEPTH} levels"
            ));
        }
    }
    if let Some(pattern) = object.get("pattern") {
        let Some(pat) = pattern.as_str() else {
            return Err("pattern must be a string".into());
        };
        if pat.len() > MAX_REGEX_PATTERN_BYTES {
            return Err(format!("pattern exceeds {MAX_REGEX_PATTERN_BYTES} bytes"));
        }
        *patterns += 1;
        if *patterns > MAX_SCHEMA_PATTERNS {
            return Err(format!("schema exceeds {MAX_SCHEMA_PATTERNS} patterns"));
        }
        if let Err(err) = cached_answer_regex(pat) {
            return Err(format!("invalid schema pattern: {err}"));
        }
    }
    let minimum = optional_finite_f64(object, "minimum")
        .ok_or_else(|| "minimum must be a finite number".to_string())?;
    let maximum = optional_finite_f64(object, "maximum")
        .ok_or_else(|| "maximum must be a finite number".to_string())?;
    if let (Some(min), Some(max)) = (minimum, maximum) {
        if min > max {
            return Err(format!("minimum ({min}) exceeds maximum ({max})"));
        }
    }
    check_u64_bounds(object, "minLength", "maxLength")?;
    check_u64_bounds(object, "minItems", "maxItems")?;
    Ok(())
}

/// Whether `value` satisfies a schema accepted by [`check_schema`].
#[must_use]
pub fn value_matches_schema(value: &Value, schema: &Value) -> bool {
    value_matches_schema_at(value, schema, 0)
}

fn value_matches_schema_at(value: &Value, schema: &Value, depth: usize) -> bool {
    if depth > MAX_MATCH_DEPTH {
        return false;
    }
    let Some(object) = schema.as_object() else {
        return false;
    };
    if let Some(expected) = object.get("const") {
        if !json_values_equal(value, expected) {
            return false;
        }
    }
    if let Some(enum_vals) = object.get("enum") {
        let Some(arr) = enum_vals.as_array() else {
            return false;
        };
        if !arr.iter().any(|item| json_values_equal(item, value)) {
            return false;
        }
    }
    if let Some(kind) = object.get("type") {
        let typed = match kind {
            Value::String(s) => is_valid_schema_type(s) && value_matches_type(value, s),
            Value::Array(items) => {
                !items.is_empty()
                    && items
                        .iter()
                        .all(|item| item.as_str().is_some_and(is_valid_schema_type))
                    && items
                        .iter()
                        .any(|item| item.as_str().is_some_and(|k| value_matches_type(value, k)))
            }
            _ => false,
        };
        if !typed {
            return false;
        }
    }
    let required = match object.get("required") {
        Some(v) => {
            let Some(arr) = v.as_array() else {
                return false;
            };
            if !arr.iter().all(Value::is_string) {
                return false;
            }
            Some(arr)
        }
        None => None,
    };
    let properties = match object.get("properties") {
        Some(v) => {
            let Some(map) = v.as_object() else {
                return false;
            };
            Some(map)
        }
        None => None,
    };
    let additional_properties = match object.get("additionalProperties") {
        Some(v) if v.is_boolean() || v.is_object() => Some(v),
        Some(_) => return false,
        None => None,
    };
    let items_schema = match object.get("items") {
        Some(v) if v.is_object() => Some(v),
        Some(_) => return false,
        None => None,
    };
    let Some(min_items) = optional_u64(object, "minItems") else {
        return false;
    };
    let Some(max_items) = optional_u64(object, "maxItems") else {
        return false;
    };
    let Some(min_length) = optional_u64(object, "minLength") else {
        return false;
    };
    let Some(max_length) = optional_u64(object, "maxLength") else {
        return false;
    };
    let pattern_re = match object.get("pattern") {
        Some(v) => {
            let Some(pat) = v.as_str() else {
                return false;
            };
            match cached_answer_regex(pat) {
                Ok(re) => Some(re),
                Err(_) => return false,
            }
        }
        None => None,
    };
    let Some(minimum) = optional_finite_f64(object, "minimum") else {
        return false;
    };
    let Some(maximum) = optional_finite_f64(object, "maximum") else {
        return false;
    };

    match value {
        Value::Object(map) => {
            if let Some(required) = required {
                for key in required {
                    let Some(key) = key.as_str() else {
                        return false;
                    };
                    if !map.contains_key(key) {
                        return false;
                    }
                }
            }
            if let Some(properties) = properties {
                for (key, sub) in properties {
                    if let Some(child) = map.get(key) {
                        if !value_matches_schema_at(child, sub, depth + 1) {
                            return false;
                        }
                    }
                }
            }
            match additional_properties {
                Some(Value::Bool(false)) => {
                    if map
                        .keys()
                        .any(|key| properties.is_none_or(|props| !props.contains_key(key)))
                    {
                        return false;
                    }
                }
                Some(sub) if sub.is_object() => {
                    for (key, child) in map {
                        if properties.is_none_or(|props| !props.contains_key(key))
                            && !value_matches_schema_at(child, sub, depth + 1)
                        {
                            return false;
                        }
                    }
                }
                _ => {}
            }
        }
        Value::Array(items) => {
            let len = items.len() as u64;
            if min_items.is_some_and(|min| len < min) {
                return false;
            }
            if max_items.is_some_and(|max| len > max) {
                return false;
            }
            if let Some(item_schema) = items_schema {
                if items
                    .iter()
                    .any(|item| !value_matches_schema_at(item, item_schema, depth + 1))
                {
                    return false;
                }
            }
        }
        Value::String(text) => {
            let len = text.chars().count() as u64;
            if min_length.is_some_and(|min| len < min) {
                return false;
            }
            if max_length.is_some_and(|max| len > max) {
                return false;
            }
            if let Some(re) = pattern_re {
                if !re.is_match(text) {
                    return false;
                }
            }
        }
        Value::Number(number) => {
            let Some(number) = number.as_f64() else {
                return false;
            };
            if minimum.is_some_and(|min| number < min) {
                return false;
            }
            if maximum.is_some_and(|max| number > max) {
                return false;
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
            cached_answer_regex(expect).is_ok_and(|re| re.is_match(answer.trim()))
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

    #[test]
    fn union_type_array_validation_and_matching() {
        let schema = json!({"type": ["string", "null"]});
        assert!(check_schema(&schema).is_ok());
        assert!(value_matches_schema(&json!("ok"), &schema));
        assert!(value_matches_schema(&json!(null), &schema));
        assert!(!value_matches_schema(&json!(42), &schema));

        assert!(check_schema(&json!({"type": []})).is_err());
        assert!(check_schema(&json!({"type": ["string", "string"]})).is_err());
        assert!(check_schema(&json!({"type": ["string", 123]})).is_err());
        assert!(check_schema(&json!({"type": ["string", "date"]})).is_err());
    }

    #[test]
    fn const_and_enum_use_lossless_numeric_equality() {
        let const_five = json!({"const": 5});
        assert!(check_schema(&const_five).is_ok());
        assert!(value_matches_schema(&json!(5), &const_five));
        assert!(value_matches_schema(&json!(5.0), &const_five));
        assert!(!value_matches_schema(&json!(5.5), &const_five));

        let const_five_float = json!({"const": 5.0});
        assert!(check_schema(&const_five_float).is_ok());
        assert!(value_matches_schema(&json!(5), &const_five_float));
        assert!(value_matches_schema(&json!(5.0), &const_five_float));

        let big_const = json!({"const": 9_007_199_254_740_992_i64});
        assert!(check_schema(&big_const).is_ok());
        assert!(value_matches_schema(
            &json!(9_007_199_254_740_992_i64),
            &big_const
        ));
        assert!(!value_matches_schema(
            &json!(9_007_199_254_740_993_i64),
            &big_const
        ));

        let big_enum = json!({"enum": [5, 9_007_199_254_740_992_i64, {"nested": [1]}]});
        assert!(check_schema(&big_enum).is_ok());
        assert!(value_matches_schema(&json!(5.0), &big_enum));
        assert!(value_matches_schema(
            &json!(9_007_199_254_740_992_i64),
            &big_enum
        ));
        assert!(value_matches_schema(
            &json!(9_007_199_254_740_992.0_f64),
            &big_enum
        ));
        assert!(!value_matches_schema(
            &json!(9_007_199_254_740_993_i64),
            &big_enum
        ));
        assert!(value_matches_schema(&json!({"nested": [1.0]}), &big_enum));

        assert!(check_schema(&json!({"enum": []})).is_err());
    }

    #[test]
    fn pattern_validation_limits_and_matching() {
        let schema = json!({"type": "string", "pattern": "^[A-Z]{2}-[0-9]{3}$"});
        assert!(check_schema(&schema).is_ok());
        assert!(value_matches_schema(&json!("US-123"), &schema));
        assert!(!value_matches_schema(&json!("us-123"), &schema));

        assert!(check_schema(&json!({"type": "string", "pattern": "[unclosed"})).is_err());
        assert!(check_schema(&json!({"type": "string", "pattern": 42})).is_err());

        let oversized_pattern = "a".repeat(MAX_REGEX_PATTERN_BYTES + 1);
        assert!(check_schema(&json!({"type": "string", "pattern": oversized_pattern})).is_err());

        let mut props = serde_json::Map::new();
        for i in 0..=MAX_SCHEMA_PATTERNS {
            props.insert(
                format!("f{i}"),
                json!({"type": "string", "pattern": format!("^p{i}$")}),
            );
        }
        let too_many_patterns = json!({"type": "object", "properties": props});
        assert!(check_schema(&too_many_patterns).is_err());
    }

    #[test]
    fn min_max_items_and_inverted_bounds_are_enforced() {
        let schema = json!({
            "type": "array",
            "items": {"type": "string"},
            "minItems": 1,
            "maxItems": 2
        });
        assert!(check_schema(&schema).is_ok());
        assert!(!value_matches_schema(&json!([]), &schema));
        assert!(value_matches_schema(&json!(["a"]), &schema));
        assert!(value_matches_schema(&json!(["a", "b"]), &schema));
        assert!(!value_matches_schema(&json!(["a", "b", "c"]), &schema));

        assert!(check_schema(&json!({"type": "string", "minLength": 5, "maxLength": 2})).is_err());
        assert!(check_schema(&json!({"type": "array", "minItems": 3, "maxItems": 1})).is_err());
        assert!(check_schema(&json!({"type": "number", "minimum": 10.0, "maximum": 5.0})).is_err());
    }

    #[test]
    fn deep_programmatic_schemas_and_values_return_false_without_overflow() {
        let mut deep_schema = json!({"type": "object"});
        let mut deep_value = json!({});
        for _ in 0..45 {
            deep_schema = json!({
                "type": "object",
                "properties": { "next": deep_schema }
            });
            deep_value = json!({ "next": deep_value });
        }
        assert!(check_schema(&deep_schema).is_err());
        assert!(!value_matches_schema(&deep_value, &deep_schema));

        let mut nested_a = json!(1);
        let mut nested_b = json!(1);
        for _ in 0..75 {
            nested_a = json!([nested_a]);
            nested_b = json!([nested_b]);
        }
        assert!(!json_values_equal(&nested_a, &nested_b));
        assert!(!value_matches_schema(
            &nested_a,
            &json!({"const": nested_b})
        ));
    }

    #[test]
    fn malformed_unvalidated_schemas_fail_closed() {
        assert!(!value_matches_schema(&json!("a"), &json!({"type": 123})));
        assert!(!value_matches_schema(&json!("a"), &json!({"type": []})));
        assert!(!value_matches_schema(
            &json!("a"),
            &json!({"type": ["string", 123]})
        ));
        assert!(!value_matches_schema(&json!("a"), &json!({"pattern": 42})));
        assert!(!value_matches_schema(&json!(123), &json!({"pattern": 42})));
        assert!(!value_matches_schema(
            &json!("a"),
            &json!({"pattern": "[unclosed"})
        ));
        assert!(!value_matches_schema(&json!([]), &json!({"minItems": -1})));
        assert!(!value_matches_schema(&json!("a"), &json!({"minItems": -1})));
        assert!(!value_matches_schema(&json!("a"), &json!({"enum": "bad"})));
        assert!(!value_matches_schema(&json!([]), &json!({"items": 123})));
        assert!(!value_matches_schema(
            &json!({}),
            &json!({"additionalProperties": 123})
        ));
    }

    #[test]
    fn regex_cache_evicts_fifo_and_stays_bounded() {
        let total = MAX_CACHED_REGEXES + 16;
        for i in 0..total {
            let pat = format!("^llmrace_fifo_cache_test_{i}$");
            let re = cached_answer_regex(&pat).expect("valid regex");
            assert!(re.is_match(&format!("llmrace_fifo_cache_test_{i}")));
        }

        let oversized = "a".repeat(MAX_REGEX_PATTERN_BYTES + 1);
        let long_re = cached_answer_regex(&oversized).expect("oversized regex compiles");
        assert!(long_re.is_match(&oversized));

        assert!(cached_answer_regex("[invalid").is_err());

        let guard = regex_cache().read().unwrap_or_else(|e| e.into_inner());
        assert!(guard.map.len() <= MAX_CACHED_REGEXES);
        assert_eq!(guard.map.len(), guard.order.len());
        assert!(!guard.map.contains_key("^llmrace_fifo_cache_test_0$"));
        assert!(!guard.map.contains_key(&oversized));
        assert!(!guard.map.contains_key("[invalid"));
    }
}
