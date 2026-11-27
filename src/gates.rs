//! CI quality gates over race results.
//!
//! The binary's `run` module consumes these from the CLI; exposing them in the
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
///
/// A non-finite or negative threshold fails closed: every successful lane is
/// reported as an offender so invalid thresholds (`NaN`, `Inf`, `< 0`) cannot
/// silently pass the gate.
#[must_use]
pub fn ttfb_offenders(summaries: &[LaneSummary], max_ms: f64) -> Vec<TtfbOffender> {
    let invalid_threshold = !max_ms.is_finite() || max_ms < 0.0;
    summaries
        .iter()
        .filter(|s| {
            s.successful()
                && (invalid_threshold || !s.mean_ttfb_ms().is_finite() || s.mean_ttfb_ms() > max_ms)
        })
        .map(|s| TtfbOffender {
            name: s.name.clone(),
            model: s.model.clone(),
            ttfb_ms: s.mean_ttfb_ms(),
        })
        .collect()
}

/// `true` when at least one lane raced but none succeeded.
///
/// Without this, `--max-ttfb` and `--baseline` gates would pass on a run
/// where every provider errored — a green pipeline over a total outage.
#[must_use]
pub fn all_lanes_failed(summaries: &[LaneSummary]) -> bool {
    !summaries.is_empty() && summaries.iter().all(|s| !s.successful())
}

/// Renders the `--max-ttfb` gate outcome: failure lines to stderr level
/// (callers print them), pass line only when not quiet.
#[must_use]
pub fn render_ttfb_verdict(offenders: &[TtfbOffender], max_ms: f64, quiet: bool) -> Verdict {
    if !max_ms.is_finite() || max_ms < 0.0 {
        return Verdict::Fail(format!("✗ TTFB GATE FAILED (invalid threshold: {max_ms})"));
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bench::RunMetrics;

    #[test]
    fn non_finite_or_negative_ttfb_threshold_fails_closed() {
        let lane = LaneSummary {
            name: "fast".into(),
            model: "m".into(),
            runs: vec![RunMetrics {
                total_ms: 50.0,
                ttfb_ms: 10.0,
                tokens: 10,
                ..RunMetrics::default()
            }],
            ..LaneSummary::default()
        };
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0] {
            let offenders = ttfb_offenders(std::slice::from_ref(&lane), bad);
            assert_eq!(offenders.len(), 1, "bad threshold {bad} must fail closed");
            assert!(render_ttfb_verdict(&[], bad, false).failed());
            assert!(render_ttfb_verdict(&offenders, bad, false).failed());
        }
    }
}
