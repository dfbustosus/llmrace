//! Report generation: markdown, JSON and CSV exports of race results.

use crate::bench::LaneSummary;
use crate::cost;

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
                .unwrap_or("");
            out.push_str(&format!("\n## variant: `{name}` — {desc}\n"));
        }
        out.push_str(&variant_table(&lanes));
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
    let ranked = rank_lanes(lanes);
    let mut out = String::new();
    out.push_str("\n| # | Provider | Model | Concurrency | Latency (mean ± 95% CI) | p50 | p95 | TTFB | Tokens/s | Cost/run |\n");
    out.push_str("|--|----------|-------|------------:|------------------------:|----:|----:|-----:|---------:|---------:|\n");
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
        out.push_str(&format!(
            "| {} | {}{} | `{}` | {} | {} | {:.0} ms | {:.0} ms | {:.0} ms | **{:.1}** | {} |\n",
            i + 1,
            s.name,
            outlier_mark,
            s.model,
            s.concurrency,
            mean,
            s.median_total_ms(),
            s.p95_total_ms(),
            s.mean_ttfb_ms(),
            s.mean_tokens_per_sec(),
            cost
        ));
    }
    out
}

fn rank_lanes<'a>(lanes: &[&'a LaneSummary]) -> Vec<&'a LaneSummary> {
    let mut ranked: Vec<&LaneSummary> = lanes.iter().filter(|s| s.successful()).copied().collect();
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
                "mean_tokens_per_sec": s.mean_tokens_per_sec(),
                "outlier_runs": s.outlier_runs(),
                "cost_per_run_usd": s.cost_per_run_usd(),
                "tool_validity": s.tool_validity(),
            })
        })
        .collect();
    serde_json::to_string_pretty(&serde_json::json!({
        "tool": "llmrace",
        "version": env!("CARGO_PKG_VERSION"),
        "prompt": prompt,
        "runs": runs,
        "max_tokens": max_tokens,
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
                s.name,
                s.model,
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

/// Renders the compact terminal ranking table.
pub fn terminal_table(summaries: &[LaneSummary]) -> String {
    let ranked = rank(summaries);
    let mut out = format!(
        "{:<4} {:<14} {:<26} {:>7} {:>18} {:>8} {:>8} {:>8} {:>8}\n",
        "#", "Provider", "Model", "C", "Lat (mean±CI)", "p50", "p95", "TTFB", "Tok/s"
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
        out.push_str(&format!(
            "{:<4} {:<14} {:<26} {:>7} {:>18} {:>7.0}ms {:>7.0}ms {:>6.0}ms {:>8.1}{}\n",
            i + 1,
            s.name,
            truncate(&s.model, 26),
            s.concurrency,
            mean,
            s.median_total_ms(),
            s.p95_total_ms(),
            s.mean_ttfb_ms(),
            s.mean_tokens_per_sec(),
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
    out
}

fn rank(summaries: &[LaneSummary]) -> Vec<&LaneSummary> {
    let mut ranked: Vec<&LaneSummary> = summaries.iter().filter(|s| s.successful()).collect();
    ranked.sort_by(|a, b| {
        b.mean_tokens_per_sec()
            .partial_cmp(&a.mean_tokens_per_sec())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    ranked
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max.saturating_sub(1)])
    }
}

fn today_utc() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02} UTC")
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
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
