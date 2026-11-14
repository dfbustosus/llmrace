//! CI quality gates over race results.
//!
//! [`crate::run`] consumes these from the CLI; exposing them in the
//! library lets CI pipelines and embedders apply the same checks to any
//! `LaneSummary` set.

use crate::bench::LaneSummary;

/// One lane that exceeded the TTFB threshold.
#[derive(Debug, Clone, PartialEq)]
pub struct TtfbOffender {
    /// Provider id.
    pub name: String,
    /// Model that was benchmarked.
    pub model: String,
    /// Mean TTFB in ms.
    pub ttfb_ms: f64,
}

/// Lanes whose mean TTFB exceeds `max_ms` (successful lanes only).
#[must_use]
pub fn ttfb_offenders(summaries: &[LaneSummary], max_ms: f64) -> Vec<TtfbOffender> {
    summaries
        .iter()
        .filter(|s| s.successful() && s.mean_ttfb_ms() > max_ms)
        .map(|s| TtfbOffender {
            name: s.name.clone(),
            model: s.model.clone(),
            ttfb_ms: s.mean_ttfb_ms(),
        })
        .collect()
}

/// Renders the `--max-ttfb` gate outcome: failure lines to stderr level
/// (callers print them), pass line only when not quiet.
#[must_use]
pub fn render_ttfb_verdict(offenders: &[TtfbOffender], max_ms: f64, quiet: bool) -> Verdict {
    if offenders.is_empty() {
        Verdict::Pass(if quiet {
            String::new()
        } else {
            format!("✓ TTFB gate passed (all providers under {max_ms:.0} ms)")
        })
    } else {
        let mut out = format!("✗ TTFB GATE FAILED (threshold {max_ms:.0} ms):");
        for o in offenders {
            out.push_str(&format!(
                "\n  {} — TTFB {:.0} ms exceeds threshold",
                o.name, o.ttfb_ms
            ));
        }
        Verdict::Fail(out)
    }
}

/// Outcome of a rendered gate.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// Gate passed; message may be empty when quiet.
    Pass(String),
    /// Gate failed; message lists the offenders.
    Fail(String),
}

impl Verdict {
    /// `true` when the gate failed (the process should exit non-zero).
    #[must_use]
    pub fn failed(&self) -> bool {
        matches!(self, Verdict::Fail(_))
    }

    /// The human-readable message.
    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Verdict::Pass(m) | Verdict::Fail(m) => m,
        }
    }
}
