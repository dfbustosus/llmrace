//! Built-in prompt variants for multi-shape benchmark sweeps.
//!
//! Latency characteristics differ wildly by prompt shape: a one-word answer
//! measures connection and scheduling overhead, a long generation measures
//! sustained throughput, code and JSON measure constrained decoding, and
//! reasoning prompts measure thinking budgets. `--sweep short,long` runs the
//! whole race once per requested variant.

use crate::error::{Error, Result};

/// One prompt variant: a stable name and its prompt text.
#[derive(Debug, Clone, PartialEq)]
pub struct Variant {
    /// Stable identifier used on the CLI and in reports.
    pub name: &'static str,
    /// One-line description of what the variant stresses.
    pub description: &'static str,
    /// The prompt sent to every provider.
    pub prompt: &'static str,
}

/// All built-in variants.
pub fn builtins() -> Vec<Variant> {
    vec![
        Variant {
            name: "short",
            description: "one-word answer: connection + scheduling overhead",
            prompt: "Reply with exactly one word: the capital of France.",
        },
        Variant {
            name: "long",
            description: "sustained generation throughput",
            prompt: "Write a vivid, detailed 800-word essay on the history of \
                     lighthouse keeping, from open-fire braziers to automated \
                     optics. Use concrete details and varied sentence rhythm.",
        },
        Variant {
            name: "code",
            description: "constrained code generation",
            prompt: "Write a Rust function `parse_query(url: &str) -> \
                     HashMap<String, String>` that returns the query parameters \
                     of a URL. Include the full function and a short example.",
        },
        Variant {
            name: "json",
            description: "structured output decoding",
            prompt: "Return ONLY a valid JSON object — no prose, no code fences \
                     — describing a fictional API service with keys: name \
                     (string), tags (array of exactly 5 short strings), \
                     active (boolean), uptime_days (integer).",
        },
        Variant {
            name: "reasoning",
            description: "multi-step thinking budget",
            prompt: "A bat and a ball cost $1.10 in total. The bat costs $1.00 \
                     more than the ball. How much does the ball cost? Reason \
                     step by step, then state the final answer on its own line.",
        },
    ]
}

/// Resolves a comma-separated sweep list (e.g. `short,json`) into variants.
///
/// Fails with a [`Error::Config`] naming the unknown variant and listing the
/// available ones.
/// Resolves a comma-separated sweep list (e.g. `short,json`) into variants.
///
/// Fails with a [`Error::Config`](crate::error::Error::Config) naming the
/// unknown variant and listing the available ones.
///
/// ```
/// use llmrace::variants;
///
/// let vs = variants::parse("short, json").unwrap();
/// assert_eq!(vs.len(), 2);
/// assert_eq!(vs[0].name, "short");
///
/// let err = variants::parse("nope").unwrap_err();
/// assert!(err.to_string().contains("unknown variant 'nope'"));
/// ```
pub fn parse(spec: &str) -> Result<Vec<Variant>> {
    let all = builtins();
    let mut out = Vec::new();
    for name in spec.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        match all.iter().find(|v| v.name == name) {
            Some(v) => {
                if !out.iter().any(|x: &Variant| x.name == v.name) {
                    out.push(v.clone());
                }
            }
            None => {
                return Err(Error::Config(format!(
                    "unknown variant '{name}'; available: {}",
                    all.iter().map(|v| v.name).collect::<Vec<_>>().join(", ")
                )))
            }
        }
    }
    if out.is_empty() {
        return Err(Error::Config(
            "empty --sweep list; expected comma-separated variant names".into(),
        ));
    }
    Ok(out)
}
