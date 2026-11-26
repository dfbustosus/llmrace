//! JSON Schema for `race.json` reports, so consumers can validate reports
//! with standard tooling. The shipped documentation is the canonical schema.

/// The JSON Schema (draft 2020-12) describing `llmrace --json` output.
/// Consumer contract tests validate real reports against this same document.
#[must_use]
pub fn race_report_schema() -> String {
    include_str!("../docs/schema.json").to_string()
}
