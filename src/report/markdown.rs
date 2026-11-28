//! Markdown, terminal ranking table, and latency histogram rendering.

use super::{group_by_variant, lane_label, rank, top_two_significance, verdict_line};
use crate::bench::LaneSummary;
use crate::cost;
use crate::stats::fmt_ms;

/// Renders a shareable markdown report ranked by aggregate tokens/s.
///
/// When lanes carry variant labels (a `--sweep` run), one table is emitted
/// per variant.
pub fn markdown(summaries: &[LaneSummary], prompt: &str, runs: usize, max_tokens: u64) -> String {
    let mut out = String::new();
    out.push_str("# 🏁 llmrace report\n\n");
    out.push_str(&format!(
        "- **Prompt**: `\"{}\"`\n",
        crate::util::md_escape_cell(prompt)
    ));
    out.push_str(&format!("- **Measured rounds per provider**: {}\n", runs));
    out.push_str(&format!("- **Max tokens**: {}\n", max_tokens));
    out.push_str(&format!("- **Generated**: {}\n", today_utc()));

    for (variant, lanes) in group_by_variant(summaries) {
        if let Some(name) = &variant {
            let desc = crate::variants::builtins()
                .into_iter()
                .find(|v| v.name == *name)
                .map(|v| v.description)
                .unwrap_or("custom variant (llmrace.toml)");
            out.push_str(&format!(
                "\n## variant: `{}` — {desc}\n",
                crate::util::md_escape_cell(name)
            ));
        }
        out.push_str(&variant_table(&lanes));
    }

    if let Some((a, b, delta, p, significant)) = top_two_significance(summaries) {
        let a = crate::util::md_escape_cell(&a);
        let b = crate::util::md_escape_cell(&b);
        let verdict = if significant {
            format!("statistically significant (p = {p:.3})")
        } else {
            format!("**not** statistically significant (p = {p:.2}) — the gap could be noise")
        };
        out.push_str(&format!(
            "\n*Significance*: {a} vs {b} — Δ {delta:+.0} ms mean latency, {verdict} (Welch's t-test on per-run latencies).\n"
        ));
    }
    if let Some(line) = verdict_line(summaries) {
        out.push_str(&format!("\n*{}*\n", crate::util::md_escape_cell(&line)));
    }

    let unpriced: Vec<&LaneSummary> = summaries
        .iter()
        .filter(|s| s.successful() && s.cost_per_run_usd().is_none())
        .collect();
    if !unpriced.is_empty() {
        out.push_str(&format!(
            "\n*no pricing data for {} lane(s) — cost column empty; pass `--price provider/model=in:out`*\n",
            unpriced.len()
        ));
    }

    let failed: Vec<&LaneSummary> = summaries.iter().filter(|s| !s.successful()).collect();
    if !failed.is_empty() {
        out.push_str("\n**Errors**\n\n");
        for s in failed {
            let variant = s
                .variant
                .as_deref()
                .map(|v| format!(" ({})", crate::util::md_escape_cell(v)))
                .unwrap_or_default();
            out.push_str(&format!(
                "- `{}{}` — {}\n",
                crate::util::md_escape_cell(&s.name),
                variant,
                crate::util::md_escape_cell(s.error.as_deref().unwrap_or("no successful rounds"))
            ));
        }
    }

    if summaries.iter().any(|s| !s.outlier_runs().is_empty()) {
        out.push_str("\n† at least one round was a 3-sigma latency outlier\n");
    }

    out.push_str(&prompt_table(summaries));
    out.push_str("\n> TTFT is time to first token, the same clock as TTFB. Tokens/s divides successful output tokens by the effective generation window: the larger of total minus TTFT and reported server time. TPOT is that window divided by output tokens. Latency, tokens/s and cost/run summarize rounds with at least one successful response; round wall time includes failed requests and retry delays. Token counts prefer provider-reported usage.\n");
    let sat_ttft: Vec<String> = summaries
        .iter()
        .filter(|s| s.successful() && s.concurrency > 1)
        .filter_map(|s| {
            let p95 = s.request_ttfb_p95_ms()?;
            let p50 = s.request_ttfb_p50_ms()?;
            Some(format!(
                "{} (p50 {} ms, p95 {} ms)",
                crate::util::md_escape_cell(&lane_label(s, summaries)),
                crate::util::md_escape_cell(&fmt_ms(p50)),
                crate::util::md_escape_cell(&fmt_ms(p95))
            ))
        })
        .collect();
    if !sat_ttft.is_empty() {
        out.push_str(&format!(
            "> Per-request TTFT under concurrency: {}.\n",
            sat_ttft.join(", ")
        ));
    }
    if summaries.iter().any(|s| s.failed_rounds() > 0) {
        out.push_str("> Fail is failed/measured rounds: any failed request marks its round as failed. JSON also reports the request-level failure rate. Rank is goodput: tokens/s × the round success rate.\n");
    }
    if summaries.iter().any(|s| s.task_pass().is_some()) {
        out.push_str("> Pass is task successes over measured rounds. A fast wrong answer is still a successful request. `--winner` counts it; a race with no task leaves the column blank and the podium unchanged.\n");
    }
    if summaries.iter().any(|s| s.retry_attempts > 0) {
        out.push_str("> Retries are extra attempts before a round settled. They are counted and not priced: a failed attempt does not return token usage.\n");
    }
    out.push_str("> Cost/run uses billed input and output tokens when the provider reported them. Cache reads and cache writes use the published discount when this crate has one for the model; hourly cache storage is not in the per-request usage and is not included. A trailing ~ means input tokens were estimated from the user prompt, which leaves out a system prompt and tool schemas. ");
    out.push_str(&crate::cost::pricing_stamp());
    out.push('\n');
    let undiscounted: Vec<String> = summaries
        .iter()
        .filter(|s| s.successful() && !s.cache_discount_applied())
        .map(|s| crate::util::md_escape_cell(s.name.as_str()))
        .collect();
    if !undiscounted.is_empty() {
        out.push_str(&format!(
            "\n*cache tokens on {} were billed at the input rate — no cached-input price is published for that model; pass `--price provider/model=in:out:cached`*\n",
            undiscounted.join(", ")
        ));
    }
    out.push_str("> Generated with [llmrace](https://github.com/dfbustosus/llmrace) 🦀\n");
    out
}

pub(crate) fn variant_table(lanes: &[&LaneSummary]) -> String {
    let ranked = rank(lanes);
    let mut out = String::new();
    out.push_str("\n| # | Provider | Model | C | Latency (mean ± 95% CI) | p50 | p95 | TTFT | Server | Network | ITL p95 | Stutters | Tokens/s | Pass | Fail | Cost/run |\n");
    out.push_str("|--|----------|-------|--:|------------------------:|----:|-----:|-----:|-------:|--------:|--------:|---------:|---------:|-----:|---------:|---------:|\n");
    for (i, s) in ranked.iter().enumerate() {
        let mean = match s.ci95_total_ms() {
            Some((lo, hi)) => format!("{:.0} ± {:.0} ms", s.mean_total_ms(), (hi - lo) / 2.0),
            None => format!("{:.0} ms", s.mean_total_ms()),
        };
        let cost = match (s.cost_per_run_usd(), s.prompt_tokens_measured()) {
            (Some(v), true) => cost::format_usd(v),
            (Some(v), false) => format!("{}~", cost::format_usd(v)),
            (None, _) => "—".to_string(),
        };
        let outlier_mark = if s.outlier_runs().is_empty() {
            ""
        } else {
            "†"
        };
        let itl_p95 = s
            .itl_p95_ms()
            .map(|v| format!("{v:.0} ms"))
            .unwrap_or_else(|| "—".into());
        let fail = format!("{}/{}", s.failed_rounds(), s.measured_rounds());
        let pass = match s.task_pass() {
            Some((ok, n)) => format!("{ok}/{n}"),
            None => "—".to_string(),
        };
        out.push_str(&format!(
            "| {} | {}{} | `{}` | {} | {} | {:.0} ms | {:.0} ms | {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
            i + 1,
            crate::util::md_escape_cell(&s.name),
            outlier_mark,
            crate::util::md_escape_cell(&s.model),
            s.concurrency,
            mean,
            s.median_total_ms(),
            s.p95_total_ms(),
            fmt_ms(s.mean_ttfb_ms()),
            s.median_server_ms()
                .map(|v| format!("{v:.0} ms"))
                .unwrap_or_else(|| "—".into()),
            s.network_overhead_ms()
                .map(|v| format!("{v:+.0} ms"))
                .unwrap_or_else(|| "—".into()),
            itl_p95,
            if s.stutters() > 0 {
                format!("{} ⏸", s.stutters())
            } else {
                "0".to_string()
            },
            if s.reasoning_tokens() > 0 {
                format!("**{:.1}** 🧠{}", s.mean_tokens_per_sec(), s.reasoning_tokens())
            } else {
                format!("**{:.1}**", s.mean_tokens_per_sec())
            },
            pass,
            fail,
            cost
        ));
    }
    out
}

/// Per-prompt rows, so a slow prompt is visible. Empty when the race
/// used one prompt.
fn prompt_table(summaries: &[LaneSummary]) -> String {
    if summaries.iter().all(|s| s.prompt_rows().is_empty()) {
        return String::new();
    }
    let mut out = String::from(
        "\n## Per prompt\n\n| Provider | Prompt | Rounds | Pass | TTFT | Tokens/s |\n|----------|--------|-------:|-----:|-----:|---------:|\n",
    );
    for s in summaries {
        let provider = crate::util::md_escape_cell(&lane_label(s, summaries));
        for row in s.prompt_rows() {
            let pass = match row.task_pass {
                Some((ok, n)) => format!("{ok}/{n}"),
                None => "—".into(),
            };
            out.push_str(&format!(
                "| {} | `{}` | {} | {pass} | {:.0} ms | {:.1} |\n",
                provider,
                crate::util::md_escape_cell(&row.id),
                row.rounds,
                row.ttfb_ms,
                row.tokens_per_sec
            ));
        }
    }
    out
}

/// Renders the compact terminal ranking table.
pub fn terminal_table(summaries: &[LaneSummary]) -> String {
    let ranked = rank(summaries);
    let any_task = summaries.iter().any(|s| s.task_pass().is_some());
    // one header/row shape, with the Pass column spliced in when the race
    // had a task
    let task_head = if any_task {
        format!(" {:>5} {:>5}", "Fail", "Pass")
    } else {
        format!(" {:>5}", "Fail")
    };
    let mut out = format!(
        "{:<4} {:<14} {:<24} {:>3} {:>16} {:>7} {:>8} {:>7} {:>8} {:>8}{} {:>7}\n",
        "#",
        "Provider",
        "Model",
        "C",
        "Lat (mean±CI)",
        "p95",
        "TTFT",
        "Server",
        "Network",
        "ITLp95",
        task_head,
        "Tok/s"
    );
    out.push_str(&"-".repeat(out.len()));
    out.push('\n');
    for (i, s) in ranked.iter().enumerate() {
        let mean = match s.ci95_total_ms() {
            Some((lo, hi)) => format!("{:.0}±{:.0}ms", s.mean_total_ms(), (hi - lo) / 2.0),
            None => format!("{:.0}ms", s.mean_total_ms()),
        };
        let mark = if s.outlier_runs().is_empty() {
            ""
        } else {
            "†"
        };
        let itl = s
            .itl_p95_ms()
            .map(|v| {
                let st = if s.stutters() > 0 {
                    format!("⏸{}", s.stutters())
                } else {
                    String::new()
                };
                format!("{v:.0}{st}")
            })
            .unwrap_or_else(|| "—".into());
        let reasoning = if s.reasoning_tokens() > 0 {
            format!(" 🧠{}", s.reasoning_tokens())
        } else {
            String::new()
        };
        let fail = format!("{}/{}", s.failed_rounds(), s.measured_rounds());
        let fail_cols = if any_task {
            let pass = s
                .task_pass()
                .map(|(ok, n)| format!("{ok}/{n}"))
                .unwrap_or_else(|| "—".into());
            format!(" {:>5} {:>5}", fail, pass)
        } else {
            format!(" {:>5}", fail)
        };
        out.push_str(&format!(
            "{:<4} {:<14} {:<24} {:>3} {:>16} {:>6.0}ms {:>6}ms {:>7} {:>8} {:>8}{} {:>8.1}{}{}\n",
            i + 1,
            s.name,
            crate::util::truncate(&s.model, 26),
            s.concurrency,
            mean,
            s.p95_total_ms(),
            fmt_ms(s.mean_ttfb_ms()),
            s.median_server_ms()
                .map(crate::stats::fmt_ms)
                .unwrap_or_else(|| "—".into()),
            s.network_overhead_ms()
                .map(|v| format!("{v:+.0}"))
                .unwrap_or_else(|| "—".into()),
            itl,
            fail_cols,
            s.mean_tokens_per_sec(),
            reasoning,
            mark
        ));
    }
    for s in summaries.iter().filter(|s| !s.successful()) {
        out.push_str(&format!(
            "  ✗ {} — {}\n",
            s.name,
            s.error.as_deref().unwrap_or("no successful rounds")
        ));
    }
    if let Some(line) = verdict_line(summaries) {
        out.push_str(&format!("{line}\n"));
    }
    if let Some((a, b, delta, p, significant)) = top_two_significance(summaries) {
        let verdict = if significant {
            format!("significant (p={p:.3})")
        } else {
            format!("NOT significant (p={p:.2}) — the gap could be noise")
        };
        out.push_str(&format!("⚖ {a} vs {b}: Δ {delta:+.0} ms, {verdict}\n"));
    }
    out
}

fn today_utc() -> String {
    format!("{} UTC", crate::util::today_utc())
}

/// Renders the latency distribution section: percentile ladder plus a
/// block-bar histogram per successful provider.
///
/// Providers need at least three measured rounds to be rendered.
pub fn histogram_section(summaries: &[LaneSummary]) -> String {
    const BUCKETS: usize = 12;
    const MAX_BAR: usize = 30;

    let lanes: Vec<&LaneSummary> = summaries
        .iter()
        .filter(|s| s.successful() && s.runs.len() >= 3)
        .collect();
    if lanes.is_empty() {
        return String::new();
    }

    let mut out = String::from("\n## Latency distribution\n\n");
    out.push_str("| Provider | p10 | p25 | p50 | p75 | p90 | p95 | p99 |\n");
    out.push_str("|--|--:|--:|--:|--:|--:|--:|--:|--:|\n");
    for s in &lanes {
        let cells: Vec<String> = s
            .latency_percentiles()
            .iter()
            .map(|(_, v)| format!("{v:.0} ms"))
            .collect();
        out.push_str(&format!(
            "| {} | {} |\n",
            crate::util::md_escape_cell(&s.name),
            cells.join(" | ")
        ));
    }

    // TTFB ladder: first-token latency distribution, distinct from the
    // time-to-last-token ladder above
    let ps = [10.0, 25.0, 50.0, 75.0, 90.0, 95.0, 99.0];
    out.push_str("\n## TTFB distribution (TTFT)\n\n");
    out.push_str("| Provider | p10 | p25 | p50 | p75 | p90 | p95 | p99 |\n");
    out.push_str("|--|--:|--:|--:|--:|--:|--:|--:|--:|\n");
    for s in &lanes {
        let sample = crate::stats::Sample::from_values(s.runs.iter().map(|r| r.ttfb_ms).collect());
        let cells: Vec<String> = sample
            .percentiles(&ps)
            .iter()
            .map(|v| format!("{v:.0} ms"))
            .collect();
        out.push_str(&format!(
            "| {} | {} |\n",
            crate::util::md_escape_cell(&s.name),
            cells.join(" | ")
        ));
    }

    out.push_str("\n```\n");
    for s in &lanes {
        let values: Vec<f64> = s.runs.iter().map(|r| r.total_ms).collect();
        let upper = s
            .latency_percentiles()
            .iter()
            .find(|(l, _)| *l == "p99")
            .map(|(_, v)| *v)
            .unwrap_or(0.0);
        let scale = if upper > 0.0 { upper } else { 1.0 };
        let width = scale / BUCKETS as f64;
        let mut histogram = [0usize; BUCKETS];
        for v in &values {
            let idx = ((v / scale) * (BUCKETS - 1) as f64).round() as usize;
            histogram[idx.min(BUCKETS - 1)] += 1;
        }
        let peak = histogram.iter().copied().max().unwrap_or(1).max(1);
        out.push_str(&format!("{}\n", crate::util::md_escape_cell(&s.name)));
        for (i, count) in histogram.iter().enumerate() {
            let bar_len = (count * MAX_BAR / peak).min(MAX_BAR);
            let bar: String = "▇".repeat(bar_len);
            out.push_str(&format!(
                "  {:>6.0}–{:<6.0} ms │{:<width$} {}\n",
                i as f64 * width,
                (i + 1) as f64 * width,
                bar,
                count,
                width = MAX_BAR
            ));
        }
    }
    out.push_str("```\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bench::RunMetrics;

    #[test]
    fn terminal_table_shows_reasoning_badge_for_non_openai_models() {
        let gemini = LaneSummary {
            name: "gemini".into(),
            model: "gemini-2.5-flash".into(),
            concurrency: 1,
            runs: vec![RunMetrics {
                total_ms: 500.0,
                ttfb_ms: 100.0,
                tokens: 50,
                reasoning_tokens: Some(300),
                reasoning_billed_separately: true,
                ..RunMetrics::default()
            }],
            ..LaneSummary::default()
        };
        let table = terminal_table(&[gemini]);
        assert!(table.contains("🧠300"), "{table}");
    }

    #[test]
    fn markdown_escapes_untrusted_provider_model_prompt_and_error() {
        let ok = LaneSummary {
            name: "p|one".into(),
            model: "m`1".into(),
            concurrency: 1,
            runs: vec![RunMetrics {
                total_ms: 100.0,
                ttfb_ms: 10.0,
                tokens: 20,
                prompt_id: "id|1".into(),
                ..RunMetrics::default()
            }],
            ..LaneSummary::default()
        };
        let err = LaneSummary {
            name: "bad|lane".into(),
            model: "m".into(),
            error: Some("boom\n<script>|x".into()),
            ..LaneSummary::default()
        };
        let md = markdown(&[ok, err], "prompt `with` | <tag>", 1, 32);
        assert!(md.contains("p\\|one"), "{md}");
        assert!(md.contains("m\\`1"), "{md}");
        assert!(md.contains("id\\|1"), "{md}");
        assert!(md.contains("&lt;script&gt;\\|x"), "{md}");
        assert!(md.contains("prompt \\`with\\` \\| &lt;tag&gt;"), "{md}");
    }
}
