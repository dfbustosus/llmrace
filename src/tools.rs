//! Tool-call benchmarking: a shared weather-function task, per-wire tool
//! schemas, and argument validation.

use crate::provider::Wire;
use serde_json::{json, Value};

/// The user prompt for the tool-calling task.
pub fn tool_task_prompt() -> &'static str {
    "What is the weather in Paris right now? Use the provided tool."
}

/// Builds the wire-specific `tools` request field for the weather task.
pub fn weather_tool(wire: Wire) -> Value {
    let parameters = json!({
        "type": "object",
        "properties": {
            "city": {"type": "string", "description": "City name"},
            "units": {"type": "string", "enum": ["celsius", "fahrenheit"]}
        },
        "required": ["city"]
    });
    match wire {
        Wire::OpenAiCompat => json!([{
            "type": "function",
            "function": {
                "name": "get_weather",
                "description": "Get the current weather for a city",
                "parameters": parameters,
            }
        }]),
        Wire::Anthropic => json!([{
            "name": "get_weather",
            "description": "Get the current weather for a city",
            "input_schema": parameters,
        }]),
        Wire::Gemini => json!([{
            "function_declarations": [{
                "name": "get_weather",
                "description": "Get the current weather for a city",
                "parameters": parameters,
            }]
        }]),
        Wire::Cohere => json!([{
            "type": "function",
            "function": {
                "name": "get_weather",
                "description": "Get the current weather for a city",
                "parameters": parameters,
            }
        }]),
        Wire::Mock => json!([]),
    }
}

/// Validates accumulated tool-call arguments as JSON containing a `city`.
pub fn validate_args(args: &str) -> bool {
    let Ok(v) = serde_json::from_str::<Value>(args) else {
        return false;
    };
    v.get("city")
        .and_then(Value::as_str)
        .is_some_and(|c| !c.is_empty())
}
