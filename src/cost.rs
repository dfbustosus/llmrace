//! Per-model pricing and prompt-token estimation for cost reporting.

use crate::error::{Error, Result};
use std::collections::HashMap;
use std::sync::OnceLock;

type Overrides = HashMap<String, (f64, f64)>;

fn overrides() -> &'static std::sync::RwLock<Overrides> {
    static OVERRIDES: OnceLock<std::sync::RwLock<Overrides>> = OnceLock::new();
    OVERRIDES.get_or_init(|| std::sync::RwLock::new(HashMap::new()))
}

/// Installs user-supplied price overrides (`provider/model` → `(input,
/// output)` USD per million tokens). Values take precedence over the
/// built-in table.
pub fn set_overrides(pairs: Vec<(String, f64, f64)>) {
    let mut map = overrides().write().expect("price overrides lock");
    for (key, pin, pout) in pairs {
        map.insert(key, (pin, pout));
    }
}

/// Parses a `--price` argument: `provider/model=1.25:5`.
pub fn parse_override(raw: &str) -> Result<(String, f64, f64)> {
    let (key, prices) = raw.split_once('=').ok_or_else(|| {
        Error::Config(format!(
            "invalid --price '{raw}': expected provider/model=in:out"
        ))
    })?;
    let (pin, pout) = prices
        .split_once(':')
        .ok_or_else(|| Error::Config(format!("invalid --price '{raw}': expected in:out prices")))?;
    let pin: f64 = pin
        .trim()
        .parse()
        .map_err(|_| Error::Config(format!("invalid input price in '{raw}'")))?;
    let pout: f64 = pout
        .trim()
        .parse()
        .map_err(|_| Error::Config(format!("invalid output price in '{raw}'")))?;
    if pin < 0.0 || pout < 0.0 {
        return Err(Error::Config(format!("negative price in '{raw}'")));
    }
    Ok((key.trim().to_ascii_lowercase(), pin, pout))
}

/// Returns `(input, output)` USD price per million tokens for a provider/model
/// pair, when known. `provider` is the lane id and `model` the model id.
///
/// ```
/// use llmrace::cost::pricing;
///
/// let (input, output) = pricing("openai", "gpt-4o-mini").unwrap();
/// assert_eq!((input, output), (0.15, 0.60));
/// assert!(pricing("ollama", "llama3.2").is_none());
/// ```
pub fn pricing(provider: &str, model: &str) -> Option<(f64, f64)> {
    let m = model.to_ascii_lowercase();
    let key = format!("{}/{}", provider, m);
    if let Ok(map) = overrides().read() {
        if let Some(v) = map.get(&format!("{}/{}", provider, model.to_ascii_lowercase())) {
            return Some(*v);
        }
    }
    let table: &[(&str, f64, f64)] = &[
        ("openai/gpt-4o", 2.50, 10.00),
        ("openai/gpt-4o-mini", 0.15, 0.60),
        ("openai/gpt-4.1", 2.00, 8.00),
        ("openai/gpt-4.1-mini", 0.40, 1.60),
        ("openai/gpt-4.1-nano", 0.10, 0.40),
        ("openai/o3-mini", 1.10, 4.40),
        ("openai/gpt-5-mini", 0.25, 2.00),
        ("openai/gpt-5", 1.25, 10.00),
        ("openai/gpt-5-nano", 0.05, 0.40),
        ("anthropic/claude-3-5-haiku-latest", 0.80, 4.00),
        ("gemini/gemini-2.0-flash", 0.10, 0.40),
        ("gemini/gemini-2.0-flash-lite", 0.075, 0.30),
        ("gemini/gemini-2.5-pro", 1.25, 10.00),
        ("anthropic/claude-3-7-sonnet-latest", 3.00, 15.00),
        ("anthropic/claude-sonnet-4", 3.00, 15.00),
        ("anthropic/claude-sonnet-4-5", 3.00, 15.00),
        ("anthropic/claude-opus-4", 15.00, 75.00),
        ("groq/llama-3.3-70b-versatile", 0.59, 0.79),
        ("groq/llama-3.1-8b-instant", 0.05, 0.08),
        ("groq/mixtral-8x7b-32768", 0.24, 0.24),
        ("deepseek/deepseek-chat", 0.27, 1.10),
        ("deepseek/deepseek-reasoner", 0.55, 2.19),
        ("mistral/mistral-small-latest", 0.20, 0.60),
        ("mistral/mistral-large-latest", 2.00, 6.00),
        (
            "together/meta-llama/Llama-3.3-70B-Instruct-Turbo",
            0.88,
            0.88,
        ),
        (
            "fireworks/accounts/fireworks/models/llama-v3p3-70b-instruct",
            0.90,
            0.90,
        ),
        ("cerebras/llama-3.3-70b", 0.85, 1.20),
        ("xai/grok-3-mini", 0.30, 0.50),
        ("xai/grok-4", 3.00, 15.00),
        ("perplexity/sonar", 1.00, 1.00),
        ("perplexity/sonar-pro", 3.00, 15.00),
        ("cohere/command-a-03-2025", 2.50, 10.00),
        ("cohere/command-r-plus-08-2024", 2.50, 10.00),
        ("openrouter/openai/gpt-4o-mini", 0.15, 0.60),
    ];
    table
        .iter()
        .find(|(k, _, _)| *k == key)
        .map(|(_, pin, pout)| (*pin, *pout))
}

/// Estimates prompt token count as the maximum of a word-based and a
/// character-based heuristic.
/// Estimates prompt token count as the maximum of a word-based and a
/// character-based heuristic.
///
/// ```
/// use llmrace::cost::estimate_prompt_tokens;
///
/// let tokens = estimate_prompt_tokens("Explain zero-knowledge proofs briefly.");
/// assert!(tokens > 3.0 && tokens < 20.0, "got {tokens}");
/// ```
pub fn estimate_prompt_tokens(prompt: &str) -> f64 {
    let words = prompt.split_whitespace().count() as f64;
    let chars = prompt.chars().count() as f64;
    let by_words = words * 1.33;
    let by_chars = chars / 4.0;
    by_words.max(by_chars)
}

/// Formats a USD amount for compact display in reports.
/// Formats a USD amount for compact display in reports.
///
/// ```
/// use llmrace::cost::format_usd;
///
/// assert_eq!(format_usd(0.000234), "$0.000234");
/// assert_eq!(format_usd(1.5), "$1.500");
/// ```
pub fn format_usd(v: f64) -> String {
    if v == 0.0 {
        "0".to_string()
    } else if v < 0.01 {
        format!("${:.6}", v)
    } else {
        format!("${:.3}", v)
    }
}
