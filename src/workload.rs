//! A shared prompt workload: one JSON object per line.
//!
//! A race, a suite entry and a ramp all draw from the same file, so the
//! winner is not an artifact of one sentence. `expect` is scored after the
//! round. `match` chooses how: exact (the default), a substring, or a regex.

use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::Path;

/// How `expect` is compared to the trimmed answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExpectMatch {
    /// Trimmed text, or a JSON object or number. The default when `match` is absent.
    #[default]
    Exact,
    /// Case-sensitive substring of the trimmed answer.
    Contains,
    /// Regex against the trimmed answer. Compiled when the file is read.
    Regex,
    /// `expect` is a JSON Schema. The trimmed answer must be JSON that matches it.
    #[serde(rename = "json_schema")]
    JsonSchema,
}

impl ExpectMatch {
    /// The wire name recorded in the request hash.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Contains => "contains",
            Self::Regex => "regex",
            Self::JsonSchema => "json_schema",
        }
    }

    fn is_exact(&self) -> bool {
        *self == Self::Exact
    }
}

/// One turn in a workload conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkloadMessage {
    /// `user` or `assistant`.
    pub role: MessageRole,
    /// Turn text.
    pub content: String,
}

/// A role in a workload conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    /// A user turn.
    User,
    /// An assistant turn.
    Assistant,
}

impl MessageRole {
    /// The role name written into an OpenAI-compatible message.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }
}

/// One prompt in a workload file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkloadPrompt {
    /// Stable id. Per-prompt rows in the report use this, not the text.
    pub id: String,
    /// The user prompt sent on this round. Empty when [`messages`](Self::messages)
    /// carries the conversation; parse fills it with the last user turn.
    #[serde(default)]
    pub prompt: String,
    /// System prompt for this round only. Wins over the race-level system prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    /// Prior and current turns. When non-empty, this is the conversation sent
    /// on the wire, and it must end with a user turn.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<WorkloadMessage>,
    /// Optional expected answer. Absent means this prompt has no text check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect: Option<String>,
    /// How [`expect`](Self::expect) is compared. Absent means exact.
    #[serde(
        default,
        rename = "match",
        skip_serializing_if = "ExpectMatch::is_exact"
    )]
    pub expect_match: ExpectMatch,
    /// A tool this prompt must call. The streamed arguments are checked
    /// against `parameters`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<crate::tools::ToolDefinition>,
}

impl WorkloadPrompt {
    /// A single user turn with no expected answer.
    #[must_use]
    pub fn text(id: impl Into<String>, prompt: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            prompt: prompt.into(),
            system: None,
            messages: Vec::new(),
            expect: None,
            expect_match: ExpectMatch::Exact,
            tool: None,
        }
    }
}

/// Parses a JSONL workload. Blank lines are skipped. Ids must be unique
/// and both `id` and `prompt` must be non-empty.
pub fn parse(text: &str) -> Result<Vec<WorkloadPrompt>> {
    let mut prompts = Vec::new();
    let mut seen = BTreeSet::new();
    for (line_no, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let item: WorkloadPrompt = serde_json::from_str(line)
            .map_err(|e| Error::Config(format!("workload line {}: {e}", line_no + 1)))?;
        if item.id.trim().is_empty() {
            return Err(Error::Config(format!(
                "workload line {}: id is empty",
                line_no + 1
            )));
        }
        if item.prompt.trim().is_empty() && item.messages.is_empty() {
            return Err(Error::Config(format!(
                "workload line {}: prompt is empty",
                line_no + 1
            )));
        }
        if !item.messages.is_empty() {
            let last = item.messages.last();
            if last.is_none_or(|message| message.role != MessageRole::User) {
                return Err(Error::Config(format!(
                    "workload line {}: messages must end with a user turn",
                    line_no + 1
                )));
            }
            for message in &item.messages {
                if message.content.trim().is_empty() {
                    return Err(Error::Config(format!(
                        "workload line {}: a message is empty",
                        line_no + 1
                    )));
                }
            }
        }
        if let Some(system) = &item.system {
            if system.trim().is_empty() {
                return Err(Error::Config(format!(
                    "workload line {}: system prompt is empty",
                    line_no + 1
                )));
            }
        }
        if item.expect_match != ExpectMatch::Exact && item.expect.is_none() {
            return Err(Error::Config(format!(
                "workload line {}: {} match needs expect",
                line_no + 1,
                item.expect_match.as_str()
            )));
        }
        if item.expect_match == ExpectMatch::Regex {
            if let Some(pattern) = &item.expect {
                crate::tools::compile_answer_regex(pattern).map_err(|e| {
                    Error::Config(format!("workload line {}: invalid regex: {e}", line_no + 1))
                })?;
            }
        }
        if item.expect_match == ExpectMatch::JsonSchema {
            if let Some(schema) = &item.expect {
                let value: serde_json::Value = serde_json::from_str(schema).map_err(|e| {
                    Error::Config(format!(
                        "workload line {}: invalid json schema: {e}",
                        line_no + 1
                    ))
                })?;
                crate::tools::check_schema(&value).map_err(|e| {
                    Error::Config(format!(
                        "workload line {}: invalid json schema: {e}",
                        line_no + 1
                    ))
                })?;
            }
        }
        if let Some(tool) = &item.tool {
            if tool.name.trim().is_empty() {
                return Err(Error::Config(format!(
                    "workload line {}: tool name is empty",
                    line_no + 1
                )));
            }
            crate::tools::check_schema(&tool.parameters).map_err(|e| {
                Error::Config(format!(
                    "workload line {}: invalid tool schema: {e}",
                    line_no + 1
                ))
            })?;
        }
        let mut item = item;
        if item.prompt.trim().is_empty() {
            if let Some(last) = item.messages.last() {
                item.prompt.clone_from(&last.content);
            }
        }
        if !seen.insert(item.id.clone()) {
            return Err(Error::Config(format!(
                "workload id '{}' is repeated",
                item.id
            )));
        }
        prompts.push(item);
    }
    if prompts.is_empty() {
        return Err(Error::Config("workload file has no prompts".into()));
    }
    Ok(prompts)
}

/// Loads a JSONL workload from disk.
pub fn load(path: &Path) -> Result<Vec<WorkloadPrompt>> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| Error::Config(format!("reading workload {}: {e}", path.display())))?;
    parse(&text).map_err(|e| match e {
        Error::Config(msg) => Error::Config(format!("{} ({})", msg, path.display())),
        other => other,
    })
}

/// The prompt for this round. Warmup uses the first item. Measured rounds
/// walk the file in order and wrap, and every lane uses the same index.
pub fn prompt_for<'a>(
    single: &'a str,
    workload: &'a [WorkloadPrompt],
    warmup: usize,
    round: usize,
) -> (&'a str, &'a str) {
    if workload.is_empty() {
        return ("", single);
    }
    let index = round.saturating_sub(warmup) % workload.len();
    let item = &workload[index];
    (item.id.as_str(), item.prompt.as_str())
}
