//! Report generation: markdown, JSON and CSV exports of race results.

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
        prompt.replace('\n', " ")
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
            out.push_str(&format!("\n## variant: `{name}` — {desc}\n"));
        }
        out.push_str(&variant_table(&lanes));
    }

    if let Some((a, b, delta, p, significant)) = top_two_significance(summaries) {
        let verdict = if significant {
            format!("statistically significant (p = {p:.3})")
        } else {
            format!("**not** statistically significant (p = {p:.2}) — the gap could be noise")
        };
        out.push_str(&format!(
            "\n*Significance*: {a} vs {b} — Δ {delta:+.0} ms mean latency, {verdict} (Welch's t-test on per-run latencies).\n"
        ));
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
                .map(|v| format!(" ({v})"))
                .unwrap_or_default();
            out.push_str(&format!(
                "- `{}{}` — {}\n",
                s.name,
                variant,
                s.error.as_deref().unwrap_or("no successful rounds")
            ));
        }
    }

    if summaries.iter().any(|s| !s.outlier_runs().is_empty()) {
        out.push_str("\n† at least one round was a 3-sigma latency outlier\n");
    }

    out.push_str("\n> Tokens/s is measured over the generation window (total time minus time-to-first-byte), summed across concurrent requests per round. Token counts use provider-reported usage when available.\n");
    out.push_str("> Generated with [llmrace](https://github.com/dfbustosus/llmrace) 🦀\n");
    out
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

fn variant_table(lanes: &[&LaneSummary]) -> String {
    let ranked = rank(lanes);
    let mut out = String::new();
    out.push_str("\n| # | Provider | Model | C | Latency (mean ± 95% CI) | p50 | p95 | TTFB | Server | Network | ITL p95 | Stutters | Tokens/s | Cost/run |\n");
    out.push_str("|--|----------|-------|--:|------------------------:|----:|-----:|-----:|-------:|--------:|--------:|---------:|---------:|---------:|\n");
    for (i, s) in ranked.iter().enumerate() {
        let mean = match s.ci95_total_ms() {
            Some((lo, hi)) => format!("{:.0} ± {:.0} ms", s.mean_total_ms(), (hi - lo) / 2.0),
            None => format!("{:.0} ms", s.mean_total_ms()),
        };
        let cost = s
            .cost_per_run_usd()
            .map(cost::format_usd)
            .unwrap_or_else(|| "—".to_string());
        let outlier_mark = if s.outlier_runs().is_empty() {
            ""
        } else {
            "†"
        };
        let itl_p95 = s
            .itl_p95_ms()
            .map(|v| format!("{v:.0} ms"))
            .unwrap_or_else(|| "—".into());
        out.push_str(&format!(
            "| {} | {}{} | `{}` | {} | {} | {:.0} ms | {:.0} ms | {} | {} | {} | {} | {} | {} | {} |\n",
            i + 1,
            s.name,
            outlier_mark,
            s.model,
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
            cost
        ));
    }
    out
}

/// Lanes ranked best-first by aggregate throughput; failed lanes drop out.
fn rank<L: std::borrow::Borrow<LaneSummary>>(lanes: &[L]) -> Vec<&LaneSummary> {
    let mut ranked: Vec<&LaneSummary> = lanes
        .iter()
        .filter(|s| s.borrow().successful())
        .map(|s| s.borrow())
        .collect();
    ranked.sort_by(|a, b| {
        b.mean_tokens_per_sec()
            .partial_cmp(&a.mean_tokens_per_sec())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    ranked
}

/// Renders the full machine-readable JSON report.
pub fn json_report(
    summaries: &[LaneSummary],
    prompt: &str,
    runs: usize,
    max_tokens: u64,
) -> String {
    json_report_with_region(summaries, prompt, runs, max_tokens, None)
}

/// Renders the JSON report with an optional region tag (`--region`), used
/// by distributed probes so merged reports stay attributable.
pub fn json_report_with_region(
    summaries: &[LaneSummary],
    prompt: &str,
    runs: usize,
    max_tokens: u64,
    region: Option<&str>,
) -> String {
    let providers: Vec<serde_json::Value> = summaries
        .iter()
        .map(|s| {
            serde_json::json!({
                "provider": s.name,
                "model": s.model,
                "variant": s.variant,
                "concurrency": s.concurrency,
                "successful": s.successful(),
                "error": s.error,
                "runs": s.runs.iter().map(|r| serde_json::json!({
                    "total_ms": r.total_ms,
                    "ttfb_ms": r.ttfb_ms,
                    "tokens": r.tokens,
                    "chars": r.chars,
                    "tokens_per_sec": r.tokens_per_sec(),
                })).collect::<Vec<_>>(),
                "mean_total_ms": s.mean_total_ms(),
                "median_total_ms": s.median_total_ms(),
                "p95_total_ms": s.p95_total_ms(),
                "ci95_total_ms": s.ci95_total_ms(),
                "mean_ttfb_ms": s.mean_ttfb_ms(),
                "ttfb_p50_ms": s.ttfb_p50_ms(),
                "ttfb_p95_ms": s.ttfb_p95_ms(),
                "mean_tokens_per_sec": s.mean_tokens_per_sec(),
                "outlier_runs": s.outlier_runs(),
                "cost_per_run_usd": s.cost_per_run_usd(),
                "server_ms": s.median_server_ms(),
                "network_overhead_ms": s.network_overhead_ms(),
                "itl_p50_ms": s.itl_p50_ms(),
                "itl_p95_ms": s.itl_p95_ms(),
                "stutters": s.stutters(),
                "visible_tokens": s.visible_tokens(),
                "reasoning_tokens": s.reasoning_tokens(),
                "thinking_ms_estimate": s.thinking_ms_estimate(),
                "request_id": s.last_request_id(),
                "tool_validity": s.tool_validity(),
            })
        })
        .collect();
    serde_json::to_string_pretty(&serde_json::json!({
        "tool": "llmrace",
        "version": env!("CARGO_PKG_VERSION"),
        "region": region,
        "prompt": prompt,
        "runs": runs,
        "max_tokens": max_tokens,
        "significance": top_two_significance(summaries).map(
            |(a, b, delta_ms, p, significant)| {
                serde_json::json!({
                    "a": a,
                    "b": b,
                    "delta_ms": delta_ms,
                    "p_value": p,
                    "significant": significant,
                })
            },
        ),
        "providers": providers,
    }))
    .unwrap_or_default()
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
    // disambiguate same-provider multi-model races, mirroring the podium
    let label = |s: &LaneSummary| {
        let dup = summaries
            .iter()
            .filter(|x| x.name == s.name && x.successful())
            .count()
            > 1;
        if dup && !s.model.is_empty() {
            format!("{}/{}", s.name, s.model)
        } else {
            s.name.clone()
        }
    };
    Some((label(leader), label(runner_up), delta, p, p < 0.05))
}

/// Renders the compact terminal ranking table.
pub fn terminal_table(summaries: &[LaneSummary]) -> String {
    let ranked = rank(summaries);
    let mut out = format!(
        "{:<4} {:<14} {:<24} {:>3} {:>16} {:>7} {:>8} {:>7} {:>8} {:>8} {:>7}\n",
        "#",
        "Provider",
        "Model",
        "C",
        "Lat (mean±CI)",
        "p95",
        "TTFB",
        "Server",
        "Network",
        "ITLp95",
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
        out.push_str(&format!(
            "{:<4} {:<14} {:<24} {:>3} {:>16} {:>6.0}ms {:>6}ms {:>7} {:>8} {:>8} {:>8.1}{}{}\n",
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
    out.push_str("|--|--:|--:|--:|--:|--:|--:|--:|\n");
    for s in &lanes {
        let cells: Vec<String> = s
            .latency_percentiles()
            .iter()
            .map(|(_, v)| format!("{v:.0} ms"))
            .collect();
        out.push_str(&format!("| {} | {} |\n", s.name, cells.join(" | ")));
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
        out.push_str(&format!("{}\n", s.name));
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

/// Renders the race as a self-contained dark-theme HTML page — podium,
/// ranked table and per-run latency bars — ready to share or host.
///
/// `scored` carries the composite podium lanes (from `--winner`); pass an
/// empty slice to omit the section.
pub fn html_report(
    summaries: &[LaneSummary],
    prompt: &str,
    runs: usize,
    max_tokens: u64,
    scored: &[crate::score::Scored],
) -> String {
    let groups: Vec<(Option<String>, Vec<crate::score::Scored>)> = if scored.is_empty() {
        Vec::new()
    } else {
        vec![(None, scored.to_vec())]
    };
    html_report_grouped(summaries, prompt, runs, max_tokens, &groups)
}

/// Like [`html_report`], but with one podium per prompt-variant group —
/// sweep runs must not mix variants in a single podium.
pub fn html_report_grouped(
    summaries: &[LaneSummary],
    prompt: &str,
    runs: usize,
    max_tokens: u64,
    groups: &[(Option<String>, Vec<crate::score::Scored>)],
) -> String {
    let mut body = String::new();

    for (variant, scored) in groups {
        if scored.is_empty() {
            continue;
        }
        let medals = ["🥇", "🥈", "🥉"];
        let heading = match variant {
            Some(v) => format!("Podium · {}", crate::util::html_escape(v)),
            None => "Podium".to_string(),
        };
        body.push_str(&format!("<section><h2>{heading}</h2><ol class='podium'>"));
        for (i, s) in scored.iter().enumerate() {
            let medal = medals.get(i).copied().unwrap_or("·");
            body.push_str(&format!(
                "<li><span class='medal'>{medal}</span> {} <span class='score'>{:.1}</span></li>",
                crate::util::html_escape(&s.name),
                s.score
            ));
        }
        body.push_str("</ol></section>");
    }

    body.push_str("<section><h2>Results</h2><table><tr><th>#</th><th>Provider</th><th>Model</th><th>C</th><th>Latency (mean ± 95% CI)</th><th>p95</th><th>TTFB</th><th>ITL p95</th><th>Tokens/s</th></tr>");
    for (i, s) in rank(summaries).iter().enumerate() {
        let ci = match s.ci95_total_ms() {
            Some((lo, hi)) => format!("±{:.0}", (hi - lo) / 2.0),
            None => "—".to_string(),
        };
        body.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{:.0} ms {}</td><td>{:.0} ms</td><td>{}</td><td>{}</td><td><b>{:.1}</b></td></tr>",
            i + 1,
            crate::util::html_escape(&s.name),
            crate::util::html_escape(&s.model),
            s.concurrency,
            s.mean_total_ms(),
            ci,
            s.p95_total_ms(),
            fmt_ms(s.mean_ttfb_ms()),
            s.itl_p95_ms().map(|v| format!("{v:.0} ms")).unwrap_or_else(|| "—".into()),
            s.mean_tokens_per_sec(),
        ));
    }
    body.push_str("</table></section>");

    let lanes: Vec<&LaneSummary> = summaries.iter().filter(|s| s.successful()).collect();
    if !lanes.is_empty() {
        body.push_str("<section><h2>Per-run latency</h2>");
        for s in &lanes {
            body.push_str(&lane_bars(s));
        }
        body.push_str("</section>");
    }

    let failures: Vec<&LaneSummary> = summaries.iter().filter(|s| !s.successful()).collect();
    if !failures.is_empty() {
        body.push_str("<section><h2>Errors</h2><ul>");
        for s in &failures {
            body.push_str(&format!(
                "<li>{} — {}</li>",
                crate::util::html_escape(&s.name),
                crate::util::html_escape(s.error.as_deref().unwrap_or("no successful rounds"))
            ));
        }
        body.push_str("</ul></section>");
    }

    let label: String = prompt.chars().take(120).collect();
    format!(
        "<!DOCTYPE html><html lang='en'><head><meta charset='utf-8'><meta name='viewport' content='width=device-width,initial-scale=1'>\
<title>llmrace report</title><style>\
:root {{ color-scheme: dark; }}\
body {{ font-family: ui-monospace, 'SF Mono', Menlo, monospace; background: #0d1117; color: #e6edf3; margin: 0 auto; max-width: 780px; padding: 2rem 1rem; }}\
h1 {{ color: #3390ff; }} h2 {{ border-bottom: 1px solid #21262d; padding-bottom: .3rem; }}\
section {{ margin: 2rem 0; }} table {{ border-collapse: collapse; width: 100%; }} th, td {{ border: 1px solid #21262d; padding: .3rem .6rem; text-align: left; }}\
th {{ color: #8b949e; }} svg {{ width: 100%; height: auto; background: #161b22; border: 1px solid #21262d; border-radius: 6px; }}\
text {{ fill: #8b949e; font-size: 10px; }} .ttfb {{ fill: #8b949e; }} .gen {{ fill: #3390ff; }}\
.podium {{ list-style: none; padding: 0; }} .podium li {{ padding: .25rem 0; }} .medal {{ margin-right: .5rem; }} .score {{ color: #8b949e; }}\
.empty {{ color: #8b949e; }} footer {{ color: #484f58; margin-top: 3rem; }} code {{ color: #8b949e; }}
</style></head><body>\
<h1>🏁 llmrace report</h1>\
<p class='empty'>{} runs · max {} tokens · prompt: <code>{}{}</code> · {}</p>\
{body}\
<footer>generated with <a style='color:#3390ff' href='https://github.com/dfbustosus/llmrace'>llmrace</a> v{} 🦀</footer>\
</body></html>",
        runs,
        max_tokens,
        crate::util::html_escape(&label),
        if prompt.chars().count() > 120 { "…" } else { "" },
        crate::util::today_utc(),
        env!("CARGO_PKG_VERSION"),
    )
}

/// Horizontal per-run bars: grey TTFB segment + blue generation segment.
fn lane_bars(s: &LaneSummary) -> String {
    const WIDTH: f64 = 720.0;
    const ROW_H: f64 = 22.0;
    let n = s.runs.len().max(1);
    let max_total = s.runs.iter().map(|r| r.total_ms).fold(1.0, f64::max);
    let height = ROW_H * n as f64 + 26.0;
    let mut svg = format!(
        "<svg viewBox='0 0 {WIDTH} {height}'><text x='4' y='14'>{}</text>",
        crate::util::html_escape(&s.name)
    );
    for (i, r) in s.runs.iter().enumerate() {
        let y = 22.0 + ROW_H * i as f64;
        let ttfb_w = (r.ttfb_ms / max_total) * (WIDTH - 140.0);
        let gen_w = ((r.total_ms - r.ttfb_ms).max(0.0) / max_total) * (WIDTH - 140.0);
        svg.push_str(&format!(
            "<rect class='ttfb' x='60' y='{y}' width='{ttfb_w:.1}' height='12' rx='2'/>\
             <rect class='gen' x='{:.1}' y='{y}' width='{gen_w:.1}' height='12' rx='2'/>",
            60.0 + ttfb_w
        ));
        svg.push_str(&format!(
            "<text x='{WIDTH}' y='{:.1}' text-anchor='end'>{:.0} ms</text>",
            y + 10.0,
            r.total_ms
        ));
    }
    svg.push_str("</svg>");
    svg
}
