//! Report generation: markdown, JSON, HTML, JUnit XML and CSV exports of race results.

mod html;
mod identity;
mod json;
mod junit;
mod markdown;

pub use html::{html_report, html_report_grouped, html_report_with_sampling};
pub use identity::{experiment_manifest, experiment_sha256, request_sha256, SentPrompts};
pub use json::{
    insert_experiment_json, insert_regression_json, insert_sampling_json, json_report,
    json_report_cited, json_report_with_region, render_json_report, sampling_json,
    JsonReportOptions,
};
pub use junit::{junit_xml, junit_xml_with_gate};
pub(crate) use markdown::variant_table;
pub use markdown::{histogram_section, markdown, terminal_table};

use crate::bench::LaneSummary;
use crate::stats::fmt_ms;
use identity::{citation, counted_rounds, experiment_manifest_with, hash_experiment_manifest};

/// Display label for one lane: `provider/model` when `summaries` contains
/// multiple lanes for the same provider and `summary.model` is non-empty,
/// otherwise `provider`.
pub(crate) fn lane_label(summary: &LaneSummary, summaries: &[LaneSummary]) -> String {
    let dup = summaries.iter().filter(|x| x.name == summary.name).count() > 1;
    if dup && !summary.model.is_empty() {
        format!("{}/{}", summary.name, summary.model)
    } else {
        summary.name.clone()
    }
}

/// Renders just the significance verdict line, if computable.
pub(crate) fn significance_line(summaries: &[LaneSummary]) -> Option<String> {
    top_two_significance(summaries).map(|(a, b, delta, p, significant)| {
        let a = crate::util::md_escape_cell(&a);
        let b = crate::util::md_escape_cell(&b);
        if significant {
            format!(
                "*Significance*: {a} vs {b} — \u{394} {delta:+.0} ms, statistically significant (p = {p:.3}) (Welch's t-test)."
            )
        } else {
            format!(
                "*Significance*: {a} vs {b} — \u{394} {delta:+.0} ms, **not** statistically significant (p = {p:.2}) — the gap could be noise (Welch's t-test)."
            )
        }
    })
}

/// One plain-language sentence: who leads, by how much, and whether the
/// gap is real. `None` with fewer than two successful lanes.
///
/// The table answers "what happened" in full detail; this is the first
/// question every user asks — who won, by how much, and should I trust
/// it — in words.
#[must_use]
pub fn verdict_line(summaries: &[LaneSummary]) -> Option<String> {
    let ranked = rank(summaries);
    if ranked.len() < 2 {
        return None;
    }
    let winner = ranked[0];
    let runner = ranked
        .iter()
        .skip(1)
        .find(|lane| lane.variant == winner.variant)?;
    let mut line = format!(
        "Verdict: {} leads on goodput — {:.1} tok/s, {:.0} ms mean, TTFT {} ms",
        lane_label(winner, summaries),
        winner.goodput_tokens_per_sec(),
        winner.mean_total_ms(),
        fmt_ms(winner.mean_ttfb_ms())
    );
    if let Some((ok, n)) = winner.task_pass() {
        line.push_str(&format!(", {ok}/{n} prompts passed"));
    }
    let ratio = if winner.mean_total_ms() > 0.0 {
        runner.mean_total_ms() / winner.mean_total_ms()
    } else {
        1.0
    };
    if ratio >= 1.1 {
        line.push_str(&format!(
            " — {ratio:.1}× faster than {} in mean latency",
            lane_label(runner, summaries)
        ));
    } else if ratio > 0.0 && ratio <= 1.0 / 1.1 {
        line.push_str(&format!(
            " — {:.1}× slower than {} in mean latency",
            1.0 / ratio,
            lane_label(runner, summaries)
        ));
    } else {
        line.push_str(&format!(
            " — within 10% of {}",
            lane_label(runner, summaries)
        ));
    }
    if let Some((_, _, _, p, significant)) = top_two_significance(summaries) {
        if significant {
            line.push_str(&format!(
                "; the mean-latency difference is significant (p={p:.3})"
            ));
        } else {
            line.push_str(&format!(
                "; the mean-latency difference is not significant (p={p:.2}) — more rounds would tell"
            ));
        }
    }
    line.push('.');
    Some(line)
}

fn group_by_variant(summaries: &[LaneSummary]) -> Vec<(Option<String>, Vec<&LaneSummary>)> {
    let mut order: Vec<Option<String>> = Vec::new();
    for s in summaries {
        if !order.contains(&s.variant) {
            order.push(s.variant.clone());
        }
    }
    order
        .into_iter()
        .map(|v| {
            (
                v.clone(),
                summaries.iter().filter(|s| s.variant == v).collect(),
            )
        })
        .collect()
}

/// Lanes ranked best-first by goodput (tokens/s × success rate).
///
/// Fully failed lanes drop out. With no failed rounds, goodput equals
/// tokens/s, so a clean race keeps the previous order.
fn rank<L: std::borrow::Borrow<LaneSummary>>(lanes: &[L]) -> Vec<&LaneSummary> {
    let mut ranked: Vec<&LaneSummary> = lanes
        .iter()
        .filter(|s| s.borrow().successful())
        .map(|s| s.borrow())
        .collect();
    ranked.sort_by(|a, b| {
        b.goodput_tokens_per_sec()
            .total_cmp(&a.goodput_tokens_per_sec())
            .then_with(|| {
                a.failure_rate()
                    .unwrap_or(0.0)
                    .total_cmp(&b.failure_rate().unwrap_or(0.0))
            })
    });
    ranked
}

/// Welch significance between the top two successful lanes of the same
/// variant group: `(leader, runner_up, delta_ms, p, significant)`.
///
/// `None` when fewer than two comparable lanes or samples exist — the
/// ranking then carries no statistical claim.
fn top_two_significance(summaries: &[LaneSummary]) -> Option<(String, String, f64, f64, bool)> {
    let ranked = rank(summaries);
    // compare within the leader's variant group: different prompts must
    // not be statistically compared
    let leader = ranked.first()?;
    let runner_up = ranked
        .iter()
        .skip(1)
        .find(|s| s.variant == leader.variant && s.runs.len() >= 2)?;
    if leader.runs.len() < 2 {
        return None;
    }
    let a: Vec<f64> = leader.runs.iter().map(|r| r.total_ms).collect();
    let b: Vec<f64> = runner_up.runs.iter().map(|r| r.total_ms).collect();
    let (t, df) = crate::stats::welch_t(&a, &b)?;
    let p = crate::stats::t_two_sided_p(t, df);
    let delta = leader.mean_total_ms() - runner_up.mean_total_ms();
    Some((
        lane_label(leader, summaries),
        lane_label(runner_up, summaries),
        delta,
        p,
        p < 0.05,
    ))
}

/// One-line description of the sampling controls that were actually sent.
///
/// `None` when every control was left at the provider default, so reports
/// for races that do not pin sampling stay identical to earlier versions.
#[must_use]
pub fn sampling_inline(sampling: &crate::provider::Sampling) -> Option<String> {
    if sampling.is_unset() {
        return None;
    }
    let mut parts = Vec::new();
    if let Some(t) = sampling.temperature {
        parts.push(format!("temperature {t}"));
    }
    if let Some(p) = sampling.top_p {
        parts.push(format!("top_p {p}"));
    }
    if let Some(seed) = sampling.seed {
        parts.push(format!("seed {seed}"));
    }
    if sampling.system.as_deref().is_some_and(|s| !s.is_empty()) {
        parts.push("system prompt".to_string());
    }
    if let Some(json_output) = &sampling.json_output {
        parts.push(match json_output {
            crate::provider::JsonOutput::Object => "json output".to_string(),
            crate::provider::JsonOutput::Schema(_) => "json schema".to_string(),
        });
    }
    Some(parts.join(", "))
}

/// Markdown metadata line for [`sampling_inline`].
#[must_use]
pub fn sampling_note(sampling: &crate::provider::Sampling) -> Option<String> {
    sampling_inline(sampling).map(|s| format!("- **Sampling**: {s}\n"))
}

/// Inserts a metadata note above the Generated line, or appends it when
/// the report has none. The shared way [`sampling_note`] and
/// [`cache_note`] reach a report.
pub fn insert_note(md: &mut String, note: &str) {
    if let Some(pos) = md.find("- **Generated**:") {
        md.insert_str(pos, note);
    } else {
        md.push_str(note);
    }
}

/// A one-line markdown note naming the lanes that mix cold and warm
/// rounds. `None` when every lane is uniformly cold or uniformly warm.
#[must_use]
pub fn cache_note(summaries: &[LaneSummary]) -> Option<String> {
    let mixed: Vec<String> = summaries
        .iter()
        .filter(|s| {
            let hits = s.cache_hit_rounds();
            !s.runs.is_empty() && hits > 0 && hits < s.runs.len()
        })
        .map(|s| {
            format!(
                "{} ({}/{})",
                crate::util::md_escape_cell(&s.name),
                s.cache_hit_rounds(),
                s.runs.len()
            )
        })
        .collect();
    if mixed.is_empty() {
        None
    } else {
        Some(format!(
            "- **Cache mix**: {} — some rounds hit the prefix cache and some did not, so the latency sample is not homogeneous\n",
            mixed.join(", ")
        ))
    }
}

/// Renders per-run rows as CSV.
pub fn csv(summaries: &[LaneSummary]) -> String {
    let mut out = String::from("provider,model,run,total_ms,ttfb_ms,tokens,tokens_per_sec\n");
    for s in summaries {
        for (i, r) in s.runs.iter().enumerate() {
            out.push_str(&format!(
                "{},{},{},{:.3},{:.3},{},{:.3}\n",
                crate::util::csv_field(&s.name),
                crate::util::csv_field(&s.model),
                i + 1,
                r.total_ms,
                r.ttfb_ms,
                r.tokens,
                r.tokens_per_sec()
            ));
        }
    }
    out
}
