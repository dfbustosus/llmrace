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
    for entry in &file.suites {
        if entry.max_tokens == 0 {
            return Err(Error::Config(format!(
                "suite '{}': max_tokens must be greater than 0",
                entry.name
            )));
        }
        if entry.warmup.checked_add(entry.runs).is_none() {
            return Err(Error::Config(format!(
                "suite '{}': warmup + runs overflows",
                entry.name
            )));
        }
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
        out.push_str(&format!(
            "\n## {}\n\n",
            crate::util::md_escape_cell(&r.name)
        ));
        if !r.description.is_empty() {
            out.push_str(&format!(
                "*{}*\n\n",
                crate::util::md_escape_cell(&r.description)
            ));
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
                    crate::util::md_escape_cell(&e.name),
                    crate::util::md_escape_cell(
                        e.error.as_deref().unwrap_or("no successful rounds")
                    )
                ));
            }
        }
        // significance if applicable
        if let Some(text) = crate::report::significance_line(&r.summaries) {
            out.push_str(&format!("\n{text}\n"));
        }
    }

    out.push_str(
        "\n> Suites run sequentially; each table is ranked by goodput (tokens/s × success rate). Generated with [llmrace](https://github.com/dfbustosus/llmrace) 🦀\n",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bench::LaneSummary;

    #[test]
    fn load_rejects_zero_max_tokens_and_warmup_runs_overflow() {
        let dir = std::env::temp_dir().join(format!("llmrace-suite-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);

        let zero_path = dir.join("zero.toml");
        std::fs::write(&zero_path, "[[suite]]\nname = \"bad\"\nmax_tokens = 0\n").expect("write");
        assert!(load(&zero_path).is_err());

        let overflow_path = dir.join("overflow.toml");
        // i64::MAX fits TOML integer and when both warmup and runs are huge,
        // checked_add overflows usize on 64-bit platforms if sum > usize::MAX.
        // Note: usize::MAX = 2^64 - 1, i64::MAX = 2^63 - 1, so 2 * i64::MAX < usize::MAX;
        // verify max_tokens > 0 passes and checked_add logic directly.
        std::fs::write(
            &overflow_path,
            "[[suite]]\nname = \"ok\"\nmax_tokens = 16\nruns = 2\nwarmup = 1\n",
        )
        .expect("write");
        assert!(load(&overflow_path).is_ok());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn combined_report_escapes_error_text() {
        let result = SuiteResult {
            name: "s1".into(),
            description: String::new(),
            summaries: vec![LaneSummary {
                name: "bad|p".into(),
                model: "m".into(),
                error: Some("err\n<script>|x".into()),
                ..LaneSummary::default()
            }],
            failed: true,
        };
        let md = combined_report(&[result]);
        assert!(md.contains("bad\\|p"), "{md}");
        assert!(md.contains("&lt;script&gt;\\|x"), "{md}");
    }
}
