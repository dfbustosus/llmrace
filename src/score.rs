//! Composite winner scoring: ranks lanes by a weighted combination of
//! TTFB, throughput and p95 latency.
//!
//! Each metric is normalized against the best lane (`best / value` for
//! lower-is-better metrics, `value / best` for throughput), multiplied by
//! its weight and summed into a penalty. The final score is `100` for the
//! best lane and decreases linearly as a lane drifts from best-in-class.

use crate::bench::LaneSummary;
use crate::error::{Error, Result};

/// Metric weights in percent; they need not sum to any particular value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Weights {
    /// Weight of mean TTFB (lower is better).
    pub ttfb: f64,
    /// Weight of aggregate tokens/s (higher is better).
    pub tps: f64,
    /// Weight of p95 latency (lower is better).
    pub p95: f64,
}

impl Default for Weights {
    fn default() -> Self {
        Weights {
            ttfb: 30.0,
            tps: 40.0,
            p95: 30.0,
        }
    }
}

impl Weights {
    /// Parses a `ttfb=30,tps=40,p95=30` spec. Unmentioned keys keep their
    /// defaults; unknown keys are rejected.
    ///
    /// ```
    /// use llmrace::score::Weights;
    ///
    /// let w = Weights::parse("tps=60").unwrap();
    /// assert_eq!(w.tps, 60.0);
    /// assert_eq!(w.ttfb, 30.0);
    /// assert!(Weights::parse("bogus=1").is_err());
    /// ```
    pub fn parse(spec: &str) -> Result<Self> {
        let mut w = Weights::default();
        for part in spec.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            let (key, value) = part.split_once('=').ok_or_else(|| {
                Error::Config(format!(
                    "invalid --weights entry '{part}': expected key=value"
                ))
            })?;
            let value: f64 = value
                .trim()
                .parse()
                .map_err(|_| Error::Config(format!("invalid weight '{value}' in '{part}'")))?;
            if value < 0.0 {
                return Err(Error::Config(format!("negative weight in '{part}'")));
            }
            match key.trim() {
                "ttfb" => w.ttfb = value,
                "tps" => w.tps = value,
                "p95" => w.p95 = value,
                other => {
                    return Err(Error::Config(format!(
                        "unknown weight key '{other}' (expected ttfb, tps or p95)"
                    )))
                }
            }
        }
        if w.ttfb + w.tps + w.p95 <= 0.0 {
            return Err(Error::Config(
                "all weights are zero — at least one must be positive".into(),
            ));
        }
        Ok(w)
    }

    fn total(&self) -> f64 {
        self.ttfb + self.tps + self.p95
    }
}

/// One lane's composite result.
#[derive(Debug, Clone, PartialEq)]
pub struct Scored {
    /// Provider id.
    pub name: String,
    /// Composite score: `100` for the best lane, less for the rest.
    pub score: f64,
}

/// Scores all successful lanes; returned sorted best-first.
///
/// Lanes missing a usable metric (e.g. zero tokens/s) are skipped to keep
/// ratios meaningful. Returns an empty vector when fewer than two lanes
/// qualify — a single lane cannot lose a race.
#[must_use]
pub fn score_lanes(summaries: &[LaneSummary], w: &Weights) -> Vec<Scored> {
    let lanes: Vec<&LaneSummary> = summaries
        .iter()
        .filter(|s| s.successful() && s.mean_tokens_per_sec() > 0.0 && s.mean_ttfb_ms() > 0.0)
        .collect();
    if lanes.len() < 2 {
        return Vec::new();
    }
    let best_ttfb = lanes
        .iter()
        .map(|s| s.mean_ttfb_ms())
        .fold(f64::MAX, f64::min);
    let best_tps = lanes
        .iter()
        .map(|s| s.mean_tokens_per_sec())
        .fold(f64::MIN, f64::max);
    let best_p95 = lanes
        .iter()
        .map(|s| s.p95_total_ms())
        .fold(f64::MAX, f64::min);

    let mut scored: Vec<Scored> = lanes
        .iter()
        .map(|s| {
            let penalty = w.ttfb * (s.mean_ttfb_ms() / best_ttfb)
                + w.tps * (best_tps / s.mean_tokens_per_sec())
                + w.p95 * (s.p95_total_ms() / best_p95);
            Scored {
                name: s.name.clone(),
                score: 100.0 * w.total() / penalty,
            }
        })
        .collect();
    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    scored
}

/// Renders the podium as a terminal/markdown-friendly block.
#[must_use]
pub fn podium(scored: &[Scored], w: &Weights) -> String {
    if scored.is_empty() {
        return String::new();
    }
    let medals = ["🥇", "🥈", "🥉"];
    let mut out = format!(
        "\n🏆 podium — weights: ttfb {:.0} / tps {:.0} / p95 {:.0}\n",
        w.ttfb, w.tps, w.p95
    );
    for (i, s) in scored.iter().enumerate() {
        let medal = medals.get(i).copied().unwrap_or(" ");
        out.push_str(&format!("  {medal} {:<14} score {:.1}\n", s.name, s.score));
    }
    out
}
