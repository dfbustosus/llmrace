//! A shared prompt workload: one JSON object per line.
//!
//! A race, a suite entry and a ramp all draw from the same file, so the
//! winner is not an artifact of one sentence. `expect` is kept for a later
//! exact-match check and is not scored here.

use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::Path;

/// One prompt in a workload file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkloadPrompt {
    /// Stable id. Per-prompt rows in the report use this, not the text.
    pub id: String,
    /// The user prompt sent on this round.
    pub prompt: String,
    /// Optional exact answer. Present so a workload can name the expected
    /// short answer; scoring it is a separate step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect: Option<String>,
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
        if item.prompt.trim().is_empty() {
            return Err(Error::Config(format!(
                "workload line {}: prompt is empty",
                line_no + 1
            )));
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
