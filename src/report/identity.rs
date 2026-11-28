//! Request and experiment identity hashing and workload citation helpers.

use crate::bench::LaneSummary;

/// The prompts a race sent, so `race.json` can name them.
#[derive(Debug, Clone)]
pub struct SentPrompts<'a> {
    /// Text a one-string race sent. A workload replaces it, round by round.
    pub prompt: &'a str,
    /// Workload drawn in measured-round order. Empty for a one-string race.
    pub workload: &'a [crate::workload::WorkloadPrompt],
    /// Warmup rounds. Measured rounds start after these, matching the engine.
    pub warmup: usize,
}

/// Hex SHA-256 of the request a race made: the prompt or workload (including
/// `expect`), `max_tokens`, sampling, the system prompt, and the tool task.
///
/// `tools` is the task name when `--tools` is set, and absent otherwise.
/// Unset sampling fields are empty. Round count is not part of the hash:
/// a longer sample of the same request stays the same request.
#[must_use]
pub fn request_sha256(
    sent_prompt: &str,
    workload: &[crate::workload::WorkloadPrompt],
    sampling: &crate::provider::Sampling,
    max_tokens: u64,
    tools: Option<&str>,
) -> String {
    let mut body = String::from("llmrace-request-v1\n");
    push_field(&mut body, "max_tokens", &max_tokens.to_string());
    push_field(&mut body, "temperature", &fmt_opt_f64(sampling.temperature));
    push_field(&mut body, "top_p", &fmt_opt_f64(sampling.top_p));
    push_field(
        &mut body,
        "seed",
        &sampling.seed.map(|s| s.to_string()).unwrap_or_default(),
    );
    push_field(
        &mut body,
        "system",
        sampling.system.as_deref().unwrap_or(""),
    );
    if let Some(json_output) = &sampling.json_output {
        let value = match json_output {
            crate::provider::JsonOutput::Object => "object".to_string(),
            crate::provider::JsonOutput::Schema(schema) => schema.to_string(),
        };
        push_field(&mut body, "json_output", &value);
    }
    push_field(&mut body, "tools", tools.unwrap_or(""));
    if workload.is_empty() {
        push_field(&mut body, "prompt", sent_prompt);
        push_field(&mut body, "workload", "");
    } else {
        push_field(&mut body, "prompt", "");
        let mut blob = String::new();
        for item in workload {
            push_field(&mut blob, "id", &item.id);
            push_field(&mut blob, "prompt", &item.prompt);
            push_field(&mut blob, "expect", item.expect.as_deref().unwrap_or(""));
            if item.expect_match != crate::workload::ExpectMatch::Exact {
                push_field(&mut blob, "match", item.expect_match.as_str());
            }
            if let Some(system) = &item.system {
                push_field(&mut blob, "system", system);
            }
            if !item.messages.is_empty() {
                let mut turns = String::new();
                for message in &item.messages {
                    push_field(&mut turns, "role", message.role.as_str());
                    push_field(&mut turns, "content", &message.content);
                }
                push_field(&mut blob, "messages", &turns);
            }
            if let Some(tool) = &item.tool {
                push_field(&mut blob, "tool", &tool.name);
                push_field(&mut blob, "tool_description", &tool.description);
                push_field(&mut blob, "tool_parameters", &tool.parameters.to_string());
            }
        }
        push_field(&mut body, "workload", &blob);
    }
    crate::provider::sha256_hex(body.as_bytes())
}

fn push_field(out: &mut String, name: &str, value: &str) {
    out.push_str(name);
    out.push(' ');
    out.push_str(&value.len().to_string());
    out.push('\n');
    out.push_str(value);
    out.push('\n');
}

/// Identity of execution settings as well as the request. Round count is
/// deliberately excluded so a longer sample can compare to its baseline.
/// Sweep variants include the actual resolved prompt texts.
pub fn experiment_sha256(
    config: &crate::bench::BenchConfig,
    variants: &[(String, String)],
) -> String {
    hash_experiment_manifest(&experiment_manifest(config, variants))
}

/// Execution settings recorded alongside the hash so it can be independently
/// reconstructed. The request itself is identified by its existing hash.
pub fn experiment_manifest(
    config: &crate::bench::BenchConfig,
    variants: &[(String, String)],
) -> serde_json::Value {
    experiment_manifest_with(config, variants)
}

pub(super) fn experiment_manifest_with<S: serde::Serialize>(
    config: &crate::bench::BenchConfig,
    variants: &[(S, S)],
) -> serde_json::Value {
    let request = request_sha256(
        &config.prompt,
        &config.workload,
        &config.sampling,
        config.max_tokens,
        config.tools.then_some(config.tool_task.as_str()),
    );
    serde_json::json!({
        "measurement_version": 2,
        "request_sha256": request,
        "concurrency": config.concurrency,
        "warmup": config.warmup,
        "warmup_mode": match config.warmup_mode {
            crate::bench::WarmupMode::SamePrompt => "same-prompt",
            crate::bench::WarmupMode::ConnectionProbe => "probe",
        },
        "retries": config.retries,
        "timeout_secs": config.timeout_secs,
        "duration_secs": config.duration_secs,
        "variants": variants,
    })
}

pub(super) fn hash_experiment_manifest(manifest: &serde_json::Value) -> String {
    crate::provider::sha256_hex(manifest.to_string().as_bytes())
}

fn fmt_opt_f64(value: Option<f64>) -> String {
    value.map(|v| format!("{v:?}")).unwrap_or_default()
}

/// Prompt ids in measured-round order, and the SHA-256 of the prompt
/// texts those rounds sent, joined with a newline.
pub(super) fn citation(sent: &SentPrompts<'_>, rounds: usize) -> (Vec<String>, String) {
    let mut ids = Vec::with_capacity(rounds);
    let mut texts = Vec::with_capacity(rounds);
    for i in 0..rounds {
        let (id, text) =
            crate::workload::prompt_for(sent.prompt, sent.workload, sent.warmup, sent.warmup + i);
        if !sent.workload.is_empty() {
            ids.push(id.to_string());
        }
        texts.push(text.to_string());
    }
    let hash = crate::provider::sha256_hex(texts.join("\n").as_bytes());
    (ids, hash)
}

pub(super) fn counted_rounds(summaries: &[LaneSummary], fallback: usize) -> usize {
    let n = summaries
        .iter()
        .map(|s| s.measured_rounds())
        .max()
        .unwrap_or(0);
    if n == 0 {
        fallback
    } else {
        n
    }
}
