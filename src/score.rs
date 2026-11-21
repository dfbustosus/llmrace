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
    /// Weight of goodput (tokens/s × success rate, higher is better).
    ///
    /// With no failed rounds this is tokens/s, so a clean race scores as before.
    pub tps: f64,
    /// Weight of p95 latency (lower is better).
    pub p95: f64,
    /// Weight of cost per run (lower is better). Applied only when every
    /// scored lane has pricing data; otherwise the weight is ignored.
    pub cost: f64,
    /// Weight of task success (higher is better). Applied only when every
    /// scored lane had a task (`expect` or `--tools`); otherwise ignored,
    /// so a plain latency race keeps the previous podium.
    pub task: f64,
}

impl Default for Weights {
    fn default() -> Self {
        Weights {
            ttfb: 30.0,
            tps: 40.0,
            p95: 30.0,
            cost: 0.0,
            task: 40.0,
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
                "cost" => w.cost = value,
                "task" => w.task = value,
                other => {
                    return Err(Error::Config(format!(
                        "unknown weight key '{other}' (expected ttfb, tps, p95, cost or task)"
                    )))
                }
            }
        }
        if w.ttfb + w.tps + w.p95 + w.cost + w.task <= 0.0 {
            return Err(Error::Config(
                "all weights are zero — at least one must be positive".into(),
            ));
        }
        Ok(w)
    }

    fn total(&self) -> f64 {
        self.ttfb + self.tps + self.p95 + self.cost + self.task
    }
}

/// Weights actually used for this race. Task is dropped when no scored
/// lane had a task, the same way cost is dropped when a lane has no price.
#[must_use]
pub fn applied_weights(summaries: &[LaneSummary], w: &Weights) -> Weights {
    let tasked = summaries.iter().any(|s| s.task_pass().is_some());
    Weights {
        task: if tasked { w.task } else { 0.0 },
        ..*w
    }
}

/// One lane's composite result.
#[derive(Debug, Clone, PartialEq)]
pub struct Scored {
    /// Display label: provider id, plus `/model` when one provider raced
    /// several models.
    pub name: String,
    /// Model that was benchmarked.
    pub model: String,
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
    score_group(
        &summaries
            .iter()
            .filter(|s| s.variant.is_none())
            .collect::<Vec<_>>(),
        w,
    )
}

/// Scores each prompt-variant group separately — variants race different
/// prompts, so their lanes must not compete head-to-head in one podium.
///
/// Returns `(variant label, scored lanes)` pairs in first-seen order;
/// plain (variant-less) races yield exactly one group labelled `None`.
#[must_use]
pub fn score_groups(summaries: &[LaneSummary], w: &Weights) -> Vec<(Option<String>, Vec<Scored>)> {
    let mut order: Vec<Option<String>> = Vec::new();
    for s in summaries {
        if !order.contains(&s.variant) {
            order.push(s.variant.clone());
        }
    }
    order
        .into_iter()
        .map(|v| {
            let group: Vec<&LaneSummary> = summaries
                .iter()
                .filter(|s| s.variant == v)
                .collect::<Vec<_>>();
            (v, score_group(&group, w))
        })
        .collect()
}

/// Shared scoring over a pre-selected group of lanes.
fn score_group(lanes: &[&LaneSummary], w: &Weights) -> Vec<Scored> {
    let lanes: Vec<&LaneSummary> = lanes
        .iter()
        .filter(|s| s.successful() && s.goodput_tokens_per_sec() > 0.0 && s.mean_ttfb_ms() > 0.0)
        .copied()
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
        .map(|s| s.goodput_tokens_per_sec())
        .fold(f64::MIN, f64::max);
    let best_p95 = lanes
        .iter()
        .map(|s| s.p95_total_ms())
        .fold(f64::MAX, f64::min);
    // Cost applies only when every scored lane has pricing; otherwise the
    // weight is dropped (and the effective total renormalized) so unpriced
    // lanes are not silently favored.
    let costs: Vec<Option<f64>> = lanes.iter().map(|s| s.cost_per_run_usd()).collect();
    let all_priced = costs.iter().all(|c| c.is_some_and(|v| v > 0.0));
    let cost_w = if all_priced { w.cost } else { 0.0 };
    let best_cost = costs.iter().filter_map(|c| *c).fold(f64::MAX, f64::min);
    let all_tasked = lanes.iter().all(|s| s.task_rate().is_some());
    let task_w = if all_tasked { w.task } else { 0.0 };
    let best_task = lanes
        .iter()
        .filter_map(|s| s.task_rate())
        .fold(0.0_f64, f64::max);
    let effective_total = w.total() - (w.cost - cost_w) - (w.task - task_w);

    let mut scored: Vec<Scored> = lanes
        .iter()
        .zip(&costs)
        .map(|(s, cost)| {
            let mut penalty = w.ttfb * (s.mean_ttfb_ms() / best_ttfb)
                + w.tps * (best_tps / s.goodput_tokens_per_sec())
                + w.p95 * (s.p95_total_ms() / best_p95);
            if cost_w > 0.0 {
                penalty += cost_w * (cost.unwrap_or(best_cost) / best_cost);
            }
            if task_w > 0.0 {
                let rate = s.task_rate().unwrap_or(0.0);
                let ratio = if best_task <= 1e-9 {
                    1.0
                } else if rate <= 1e-9 {
                    best_task / 1e-2
                } else {
                    best_task / rate
                };
                penalty += task_w * ratio;
            }
            Scored {
                name: s.name.clone(),
                model: s.model.clone(),
                score: 100.0 * effective_total / penalty,
            }
        })
        .collect();
    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    // When one provider races several models, disambiguate labels with the model.
    let duplicated: Vec<String> = scored
        .iter()
        .map(|s| s.name.clone())
        .filter(|n| scored.iter().filter(|x| x.name == *n).count() > 1)
        .collect();
    for s in &mut scored {
        if duplicated.contains(&s.name) && !s.model.is_empty() {
            s.name = format!("{}/{}", s.name, s.model);
        }
    }
    scored
}

/// Renders the podium as a terminal/markdown-friendly block. For sweep
/// runs, use [`podiums`] — one podium per variant.
#[must_use]
pub fn podium(scored: &[Scored], w: &Weights) -> String {
    if scored.is_empty() {
        return String::new();
    }
    let medals = ["🥇", "🥈", "🥉"];
    let mut out = format!(
        "\n🏆 podium — weights: ttfb {:.0} / tps {:.0} / p95 {:.0}",
        w.ttfb, w.tps, w.p95
    );
    if w.task > 0.0 {
        out.push_str(&format!(" / task {:.0}", w.task));
    }
    out.push('\n');
    for (i, s) in scored.iter().enumerate() {
        let medal = medals.get(i).copied().unwrap_or(" ");
        out.push_str(&format!("  {medal} {:<22} score {:.1}\n", s.name, s.score));
    }
    out
}

/// Renders one podium per variant group (see [`score_groups`]).
#[must_use]
pub fn podiums(groups: &[(Option<String>, Vec<Scored>)], w: &Weights) -> String {
    let mut out = String::new();
    for (variant, scored) in groups {
        let block = podium(scored, w);
        if block.is_empty() {
            continue;
        }
        match variant {
            Some(v) => out.push_str(&format!("\n\u{2014} variant `{v}` \u{2014}{block}")),
            None => out.push_str(&block),
        }
    }
    out
}
