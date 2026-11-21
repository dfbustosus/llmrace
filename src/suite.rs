//! Benchmark suite orchestration: run multiple named races from a TOML
//! definition and produce a combined report.
//!
//! ```toml
//! [[suite]]
//! name = "latency-short"
//! providers = ["openai", "groq"]
//! runs = 5
//! prompt = "Reply with one word."
//!
//! [[suite]]
//! name = "saturation"
//! providers = ["openai"]
//! concurrency = 8
//! runs = 3
//! ```
//!
//! `llmrace --suite file.toml` runs each suite sequentially and emits a
//! combined markdown report with one section per suite.

use crate::error::{Error, Result};
use serde::Deserialize;
use std::path::Path;

/// One race definition inside a suite.
#[derive(Debug, Clone, Deserialize)]
pub struct SuiteEntry {
    /// Display name for this suite section.
    pub name: String,
    /// One-line description shown in the report.
    #[serde(default)]
    pub description: String,
    /// Providers to race (CLI ids, including `provider=model` syntax).
    #[serde(default)]
    pub providers: Vec<String>,
    /// Measured rounds per provider.
    #[serde(default = "default_runs")]
    pub runs: usize,
    /// Warmup rounds (default 1).
    #[serde(default = "default_warmup")]
    pub warmup: usize,
    /// Concurrent requests per provider (default 1).
    #[serde(default = "default_concurrency")]
    pub concurrency: usize,
    /// Max tokens (default 256).
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u64,
    /// Prompt text.
    #[serde(default)]
    pub prompt: Option<String>,
    /// Prompt file path (alternative to `prompt`). The whole file is one prompt.
    #[serde(default)]
    pub prompt_file: Option<String>,
    /// JSONL workload path. One `{"id","prompt","expect"?}` per line, shared
    /// by every lane. When set, this replaces `prompt` and `prompt_file`.
    #[serde(default)]
    pub workload: Option<String>,
    /// Prompt-variant sweep spec (e.g. `"short,json"`).
    #[serde(default)]
    pub sweep: Option<String>,
    /// Enable tool-calling benchmark for this suite.
    #[serde(default)]
    pub tools: bool,
    /// Tool task name (weather, math, lookup).
    #[serde(default)]
    pub tool_task: Option<String>,
}

fn default_runs() -> usize {
    3
}
fn default_warmup() -> usize {
    1
}
fn default_concurrency() -> usize {
    1
}
fn default_max_tokens() -> u64 {
    256
}

/// Parsed suite file.
#[derive(Debug, Clone, Deserialize)]
pub struct SuiteFile {
    /// The named races to run sequentially.
    #[serde(default, rename = "suite")]
    pub suites: Vec<SuiteEntry>,
}

/// Parses a suite file from disk.
pub fn load(path: &Path) -> Result<SuiteFile> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| Error::Config(format!("reading suite file {}: {e}", path.display())))?;
    let file: SuiteFile =
        toml::from_str(&raw).map_err(|e| Error::Config(format!("parsing suite file: {e}")))?;
    if file.suites.is_empty() {
        return Err(Error::Config("suite file has no [[suite]] entries".into()));
    }
    Ok(file)
}

/// The result of running one suite entry.
#[derive(Debug, Clone)]
pub struct SuiteResult {
    /// The suite's display name.
    pub name: String,
    /// The suite's description.
    pub description: String,
    /// Lane summaries from the race.
    pub summaries: Vec<crate::bench::LaneSummary>,
    /// Exit-code-relevant failure (every lane failed).
    pub failed: bool,
}

/// Renders the combined markdown report for all suite results.
#[must_use]
pub fn combined_report(results: &[SuiteResult]) -> String {
    let mut out = String::from("# 🏁 llmrace suite report\n\n");
    let total: usize = results.len();
    let failed = results.iter().filter(|r| r.failed).count();
    out.push_str(&format!(
        "- **Suites**: {total} ({failed} failed)\n- **Generated**: {}\n",
        crate::util::today_utc()
    ));

    for r in results {
        out.push_str(&format!("\n## {}\n\n", r.name));
        if !r.description.is_empty() {
            out.push_str(&format!("*{}*\n\n", r.description));
        }
        if r.summaries.is_empty() {
            out.push_str("*no results*\n");
            continue;
        }
        // reuse the standard ranked table
        out.push_str(&crate::report::variant_table(
            &r.summaries.iter().collect::<Vec<_>>(),
        ));
        // errors
        let errs: Vec<_> = r.summaries.iter().filter(|s| !s.successful()).collect();
        if !errs.is_empty() {
            out.push_str("\n**Errors**\n\n");
            for e in errs {
                out.push_str(&format!(
                    "- `{}` — {}\n",
                    e.name,
                    e.error.as_deref().unwrap_or("no successful rounds")
                ));
            }
        }
        // significance if applicable
        if let Some(text) = crate::report::significance_line(&r.summaries) {
            out.push_str(&format!("\n{text}\n"));
        }
    }

    out.push_str(
        "\n> Suites run sequentially; each table is ranked by tokens/s. Generated with [llmrace](https://github.com/dfbustosus/llmrace) 🦀\n",
    );
    out
}
