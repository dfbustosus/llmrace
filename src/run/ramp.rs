//! Saturation-curve mode: race at each concurrency level.

use super::collect::{race_collect, SignalGuard};
use super::config::RunConfig;
use super::reports::write_atomic;
use anyhow::Result;
use llmrace::bench::LaneSummary;
use std::fmt::Write as _;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::Arc;

/// One `--ramp` level and its lane summaries.
pub(crate) struct RampLevel {
    /// Concurrency raced at this level.
    pub concurrency: usize,
    /// Lane results at this level.
    pub summaries: Vec<LaneSummary>,
}

/// Per-concurrency-level measurements for one lane in a ramp.
struct RampPoint {
    concurrency: usize,
    tps: f64,
    goodput: f64,
    request_ttfb_p50_ms: Option<f64>,
    request_ttfb_p95_ms: Option<f64>,
}

/// Aggregated saturation summary for one provider/model lane across ramp phases,
/// shared by terminal, Markdown, JSON, and HTML outputs so peak goodput and
/// post-peak degradation never drift between renderers.
struct RampRow {
    name: String,
    model: String,
    /// Per-concurrency measurements for each completed phase.
    points: Vec<RampPoint>,
    peak_concurrency: usize,
    peak_tps: f64,
    peak_goodput: f64,
    failure_rate_crosses_at: Option<usize>,
    degraded: bool,
}

impl RampRow {
    fn peak_label(&self) -> String {
        if self.degraded {
            format!(
                "c={} ({:.1}) \u{2b07}",
                self.peak_concurrency, self.peak_goodput
            )
        } else {
            format!("c={} ({:.1})", self.peak_concurrency, self.peak_goodput)
        }
    }
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

fn json_opt_round1(v: Option<f64>) -> serde_json::Value {
    match v.filter(|x| x.is_finite()) {
        Some(x) => serde_json::json!(round1(x)),
        None => serde_json::Value::Null,
    }
}

fn fmt_opt_ms(v: Option<f64>) -> String {
    v.filter(|x| x.is_finite())
        .map(llmrace::stats::fmt_ms)
        .unwrap_or_else(|| "\u{2014}".to_string())
}

fn has_request_ttfb_p95(rows: &[RampRow]) -> bool {
    rows.iter()
        .any(|row| row.points.iter().any(|p| p.request_ttfb_p95_ms.is_some()))
}

fn build_ramp_rows(phases: &[RampLevel]) -> Vec<RampRow> {
    let order: Vec<(String, String)> = phases
        .first()
        .map(|p| {
            p.summaries
                .iter()
                .map(|s| (s.name.clone(), s.model.clone()))
                .collect()
        })
        .unwrap_or_default();

    let mut rows = Vec::with_capacity(order.len());
    for (name, model) in order {
        let mut points = Vec::with_capacity(phases.len());
        let mut best: Option<(usize, f64, f64)> = None;
        let mut failure_rate_crosses_at = None;
        for phase in phases {
            let lane = phase
                .summaries
                .iter()
                .find(|s| s.name == name && s.model == model);
            let ok_lane = lane.filter(|s| s.successful());
            let tps = ok_lane.map(|s| s.mean_tokens_per_sec()).unwrap_or(0.0);
            let goodput = ok_lane.map(|s| s.goodput_tokens_per_sec()).unwrap_or(0.0);
            let request_ttfb_p50_ms = ok_lane.and_then(|s| s.request_ttfb_p50_ms());
            let request_ttfb_p95_ms = ok_lane.and_then(|s| s.request_ttfb_p95_ms());
            if best.is_none_or(|(_, _, b)| goodput > b) {
                best = Some((phase.concurrency, tps, goodput));
            }
            if failure_rate_crosses_at.is_none()
                && lane
                    .and_then(|s| s.failure_rate())
                    .is_some_and(|rate| rate > 0.05)
            {
                failure_rate_crosses_at = Some(phase.concurrency);
            }
            points.push(RampPoint {
                concurrency: phase.concurrency,
                tps,
                goodput,
                request_ttfb_p50_ms,
                request_ttfb_p95_ms,
            });
        }
        let (peak_concurrency, peak_tps, peak_goodput) = best.unwrap_or((1, 0.0, 0.0));
        // Flag degradation when any higher concurrency drops >10% below peak goodput.
        let degraded = peak_goodput > 0.0
            && points
                .iter()
                .any(|p| p.concurrency > peak_concurrency && p.goodput < peak_goodput * 0.9);
        rows.push(RampRow {
            name,
            model,
            points,
            peak_concurrency,
            peak_tps,
            peak_goodput,
            failure_rate_crosses_at,
            degraded,
        });
    }
    rows
}

fn build_ramp_json(rows: &[RampRow]) -> Vec<serde_json::Value> {
    let mut ramp_json: Vec<serde_json::Value> = Vec::with_capacity(rows.len());
    for row in rows {
        let mut per_level = serde_json::Map::new();
        let mut ttfb_p50_by_level = serde_json::Map::new();
        let mut ttfb_p95_by_level = serde_json::Map::new();
        for point in &row.points {
            let key = point.concurrency.to_string();
            per_level.insert(key.clone(), serde_json::json!(round1(point.tps)));
            ttfb_p50_by_level.insert(key.clone(), json_opt_round1(point.request_ttfb_p50_ms));
            ttfb_p95_by_level.insert(key, json_opt_round1(point.request_ttfb_p95_ms));
        }
        ramp_json.push(serde_json::json!({
            "provider": row.name,
            "model": row.model,
            "tokens_per_sec_by_concurrency": per_level,
            "request_ttfb_p50_ms_by_concurrency": ttfb_p50_by_level,
            "request_ttfb_p95_ms_by_concurrency": ttfb_p95_by_level,
            "peak_concurrency": row.peak_concurrency,
            "peak_tokens_per_sec": round1(row.peak_tps),
            "peak_goodput_tokens_per_sec": round1(row.peak_goodput),
            "failure_rate_crosses_at": row.failure_rate_crosses_at,
            "degrades_past_peak": row.degraded,
        }));
    }
    ramp_json
}

fn ramp_terminal(levels: &[usize], rows: &[RampRow]) -> String {
    let mut out = format!("{:<20}", "Provider");
    for level in levels {
        let _ = write!(out, " {:>10}", format!("c={level}"));
    }
    out.push_str("      peak\n");
    out.push_str(&"-".repeat(out.len().saturating_sub(1)));
    out.push('\n');
    for row in rows {
        let name = llmrace::util::sanitize_control_chars(&row.name);
        let _ = write!(out, "{name:<20}");
        for point in &row.points {
            let _ = write!(out, " {:>10.1}", point.tps);
        }
        let peak_note = row.peak_label();
        let _ = writeln!(out, "  {peak_note}");
    }

    if has_request_ttfb_p95(rows) {
        out.push_str("\nRequest TTFT p95 (ms)\n");
        let mut header = format!("{:<20}", "Provider");
        for level in levels {
            let _ = write!(header, " {:>10}", format!("c={level}"));
        }
        let sep_len = header.len();
        out.push_str(&header);
        out.push('\n');
        out.push_str(&"-".repeat(sep_len));
        out.push('\n');
        for row in rows {
            let name = llmrace::util::sanitize_control_chars(&row.name);
            let _ = write!(out, "{name:<20}");
            for point in &row.points {
                let cell = fmt_opt_ms(point.request_ttfb_p95_ms);
                let _ = write!(out, " {cell:>10}");
            }
            out.push('\n');
        }
    }

    out
}

fn ramp_markdown_curve(levels: &[usize], rows: &[RampRow]) -> String {
    let mut out = String::from("| Provider |");
    for level in levels {
        let _ = write!(out, " c={level} |");
    }
    out.push_str(" peak |\n|---|");
    for _ in levels {
        out.push_str("---:|");
    }
    out.push_str("---:|\n");

    for row in rows {
        let _ = write!(out, "| {} |", llmrace::util::md_escape_cell(&row.name));
        for point in &row.points {
            let _ = write!(out, " {:.1} |", point.tps);
        }
        let _ = writeln!(out, " {} |", row.peak_label());
    }

    out.push_str("\n> Peak is the concurrency where goodput is highest. The cells are tokens/s of successful rounds. \u{2b07} flags a goodput drop of more than 10% past that peak. A failure rate above 5% is `failure_rate_crosses_at` in the JSON.\n");

    if has_request_ttfb_p95(rows) {
        out.push_str("\n### Request TTFT p95 (ms)\n\n| Provider |");
        for level in levels {
            let _ = write!(out, " c={level} |");
        }
        out.push_str("\n|---|");
        for _ in levels {
            out.push_str("---:|");
        }
        out.push('\n');

        for row in rows {
            let _ = write!(out, "| {} |", llmrace::util::md_escape_cell(&row.name));
            for point in &row.points {
                let cell = llmrace::util::md_escape_cell(&fmt_opt_ms(point.request_ttfb_p95_ms));
                let _ = write!(out, " {cell} |");
            }
            out.push('\n');
        }
    }

    out
}

/// Parses a `--ramp` level list (`1,2,4,8`): values must be \u{2265} 1 and
/// at least two distinct levels must remain after sorting and deduplication.
pub(crate) fn parse_ramp_levels(spec: &str) -> anyhow::Result<Vec<usize>> {
    let mut levels: Vec<usize> = Vec::new();
    for part in spec.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let level: usize = part.parse().map_err(|_| {
            anyhow::anyhow!("invalid --ramp level '{part}' (expected integers like 1,2,4,8)")
        })?;
        if level == 0 {
            anyhow::bail!("--ramp levels must be >= 1");
        }
        levels.push(level);
    }
    levels.sort_unstable();
    levels.dedup();
    if levels.len() < 2 {
        anyhow::bail!("--ramp needs at least two distinct concurrency levels");
    }
    Ok(levels)
}

/// `--ramp`: races every provider at each concurrency level and renders the
/// saturation curve. Returns the process exit code.
pub(crate) async fn ramp_race(mut cfg: RunConfig, levels: &[usize]) -> Result<i32> {
    let max_concurrency = levels.iter().copied().max().unwrap_or(1);
    cfg.client = llmrace::provider::benchmark_client(
        std::time::Duration::from_secs(cfg.bench.timeout_secs),
        max_concurrency,
    )?;

    if !cfg.quiet {
        cfg.status(format!(
            "\u{1f3c1} llmrace v{} ramp — {} providers \u{d7} levels {:?} ({} rounds each)",
            env!("CARGO_PKG_VERSION"),
            cfg.providers.len(),
            levels,
            cfg.bench.runs
        ));
        if cfg.bench.workload.is_empty() {
            cfg.status(format!(
                "   prompt: \"{}\"\n",
                cfg.prompt.replace('\n', " ")
            ));
        } else {
            cfg.status(format!(
                "   workload: {} prompts\n",
                cfg.bench.workload.len()
            ));
        }
    }

    let mut phases: Vec<RampLevel> = Vec::new();
    // Single Ctrl-C guard across all phases: SIGINT finishes the in-flight
    // phase's rounds and skips remaining levels without leaking signal tasks.
    let signal_guard = SignalGuard::install(
        "\u{23f9} interrupt received — finishing in-flight rounds, writing the curve so far…",
    );
    let stop = signal_guard.flag();
    for (idx, level) in levels.iter().enumerate() {
        if stop.load(Ordering::Relaxed) && idx > 0 {
            break;
        }
        if !cfg.quiet {
            cfg.status(format!(
                "\u{25b8} phase {}/{} — concurrency {level}",
                idx + 1,
                levels.len()
            ));
        }
        let mut bench = cfg.bench.clone();
        bench.concurrency = *level;
        let summaries = race_collect(&cfg, Some(bench), true, Arc::clone(&stop)).await?;
        phases.push(RampLevel {
            concurrency: *level,
            summaries,
        });
    }
    drop(signal_guard);

    let completed_levels: Vec<usize> = phases.iter().map(|p| p.concurrency).collect();
    let rows = build_ramp_rows(&phases);
    let curve = ramp_markdown_curve(&completed_levels, &rows);
    let curve_term = ramp_terminal(&completed_levels, &rows);
    let ramp_json = build_ramp_json(&rows);

    if !cfg.quiet {
        cfg.status(format!("\n{curve_term}"));
    }

    // reports: markdown curve section + structured json
    let mut md = String::from("# \u{1f3c1} llmrace ramp\n\n");
    md.push_str(&format!(
        "- **Prompt**: `\"{}\"`\n- **Levels**: {:?}\n- **Rounds per level**: {}\n",
        if cfg.bench.workload.is_empty() {
            llmrace::util::md_escape_cell(&cfg.prompt)
        } else {
            format!("{} prompts", cfg.bench.workload.len())
        },
        levels,
        cfg.bench.runs
    ));
    if let Some(note) = llmrace::report::sampling_note(&cfg.bench.sampling) {
        md.push_str(&note);
    }
    md.push_str("\n## Saturation curve (tokens/s by concurrency)\n\n");
    md.push_str(&curve);

    let out_ext = if cfg.out == "-" {
        None
    } else {
        Path::new(&cfg.out)
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
    };

    if cfg.out == "-" {
        println!("{md}");
    } else {
        let md_path = match out_ext.as_deref() {
            Some("json" | "html") => crate::report_sibling(&cfg.out, "md"),
            _ => cfg.out.clone(),
        };
        write_atomic(Path::new(&md_path), md.as_bytes())?;
        cfg.status(format!("\u{1f4c4} ramp report saved to {md_path}"));
    }

    if cfg.html || out_ext.as_deref() == Some("html") {
        let h = ramp_html(&completed_levels, &rows);
        let path = crate::report_sibling(&cfg.out, "html");
        write_atomic(Path::new(&path), h.as_bytes())?;
        cfg.status(format!("\u{1f310} ramp html saved to {path}"));
    }

    if cfg.json || out_ext.as_deref() == Some("json") {
        let mut payload = serde_json::json!({
            "tool": "llmrace",
            "version": env!("CARGO_PKG_VERSION"),
            "mode": "ramp",
            "region": cfg.region,
            "prompt": cfg.prompt,
            "runs": cfg.bench.runs,
            "max_tokens": cfg.bench.max_tokens,
            "levels": levels,
            "ramp": ramp_json,
        });
        if let Some(sampling) = llmrace::report::sampling_json(&cfg.bench.sampling) {
            payload["sampling"] = sampling;
        }
        let path = crate::report_sibling(&cfg.out, "json");
        let serialized = serde_json::to_string_pretty(&payload)?;
        write_atomic(Path::new(&path), serialized.as_bytes())?;
        cfg.status(format!("\u{1f4ca} ramp json saved to {path}"));
    }

    if llmrace::gates::all_lanes_failed(
        &phases
            .last()
            .map(|p| p.summaries.clone())
            .unwrap_or_default(),
    ) {
        eprintln!("\n\u{2717} every provider failed at the highest level");
        return Ok(1);
    }
    Ok(0)
}

fn ramp_html_ttfb_table(levels: &[usize], rows: &[RampRow]) -> String {
    if !has_request_ttfb_p95(rows) {
        return String::new();
    }
    let mut out = String::from("<h2>Request TTFT p95 (ms)</h2><table><tr><th>Provider</th>");
    for level in levels {
        let _ = write!(out, "<th>c={level}</th>");
    }
    out.push_str("</tr>");
    for row in rows {
        let _ = write!(
            out,
            "<tr><td>{} ({})</td>",
            llmrace::util::html_escape(&row.name),
            llmrace::util::html_escape(&row.model)
        );
        for point in &row.points {
            let cell = llmrace::util::html_escape(&fmt_opt_ms(point.request_ttfb_p95_ms));
            let _ = write!(out, "<td>{cell}</td>");
        }
        out.push_str("</tr>");
    }
    out.push_str("</table>");
    out
}

/// Self-contained HTML page for a ramp: SVG throughput-by-concurrency
/// series per provider plus the curve table.
fn ramp_html(levels: &[usize], rows: &[RampRow]) -> String {
    const W: f64 = 760.0;
    const H: f64 = 320.0;
    const PAD: f64 = 44.0;
    const COLORS: [&str; 6] = [
        "#3390ff", "#3fb950", "#d29922", "#f778ba", "#a371f7", "#39c5cf",
    ];

    let max_tps = rows
        .iter()
        .flat_map(|row| row.points.iter().map(|p| p.tps))
        .fold(1.0, f64::max);
    let max_level = levels.last().copied().unwrap_or(1).max(1) as f64;
    let x = |level: usize| PAD + (level as f64 / max_level) * (W - 2.0 * PAD);
    let y = |v: f64| H - PAD - (v / max_tps) * (H - 2.0 * PAD);

    let mut svg = format!(
        "<svg viewBox='0 0 {W} {H}' xmlns='http://www.w3.org/2000/svg' role='img' aria-label='saturation curve'>"
    );
    // gridlines at 25% steps
    for step in 1..=3 {
        let gy = y(max_tps * step as f64 / 4.0);
        svg.push_str(&format!(
            "<line x1='{PAD}' y1='{gy:.1}' x2='{:.1}' y2='{gy:.1}' stroke='#21262d'/>",
            W - PAD
        ));
        svg.push_str(&format!(
            "<text x='{}' y='{gy:.1}'>{:.0}</text>",
            PAD - 6.0,
            max_tps * step as f64 / 4.0
        ));
    }
    for level in levels {
        svg.push_str(&format!(
            "<text x='{:.1}' y='{}' text-anchor='middle'>{level}</text>",
            x(*level),
            H - PAD + 14.0
        ));
    }
    svg.push_str(&format!(
        "<text x='{PAD}' y='{}'>c (concurrency)</text><text transform='rotate(-90)' x='{}' y='14'>tokens/s</text>",
        H - 8.0,
        -(H / 2.0),
    ));

    let mut legend = String::new();
    for (i, row) in rows.iter().enumerate() {
        let color = COLORS[i % COLORS.len()];
        let points: Vec<String> = row
            .points
            .iter()
            .map(|p| format!("{:.1},{:.1}", x(p.concurrency), y(p.tps)))
            .collect();
        svg.push_str(&format!(
            "<polyline fill='none' stroke='{color}' stroke-width='2' points='{}'/>",
            points.join(" ")
        ));
        for point in &row.points {
            svg.push_str(&format!(
                "<circle cx='{:.1}' cy='{:.1}' r='3' fill='{color}'/>",
                x(point.concurrency),
                y(point.tps)
            ));
        }
        legend.push_str(&format!(
            "<li><span style='color:{color}'>\u{25cf}</span> {} ({})</li>",
            llmrace::util::html_escape(&row.name),
            llmrace::util::html_escape(&row.model)
        ));
    }
    svg.push_str("</svg>");

    let mut table = String::from("<table><tr><th>Provider</th>");
    for level in levels {
        table.push_str(&format!("<th>c={level}</th>"));
    }
    table.push_str("<th>peak</th></tr>");
    for row in rows {
        table.push_str(&format!(
            "<tr><td>{} ({})</td>",
            llmrace::util::html_escape(&row.name),
            llmrace::util::html_escape(&row.model)
        ));
        for point in &row.points {
            table.push_str(&format!("<td>{:.1}</td>", point.tps));
        }
        table.push_str(&format!(
            "<td><b>{}</b></td></tr>",
            llmrace::util::html_escape(&row.peak_label())
        ));
    }
    table.push_str("</table>");
    let ttfb_table = ramp_html_ttfb_table(levels, rows);

    format!(
        "<!DOCTYPE html><html lang='en'><head><meta charset='utf-8'><meta name='viewport' content='width=device-width,initial-scale=1'><title>llmrace ramp</title><style>:root {{ color-scheme: dark; }}body {{ font-family: ui-monospace, 'SF Mono', Menlo, monospace; background: #0d1117; color: #e6edf3; margin: 0 auto; max-width: 820px; padding: 2rem 1rem; }}h1 {{ color: #3390ff; }} h2 {{ color: #e6edf3; font-size: 1.1rem; margin-top: 2rem; }} svg {{ width: 100%; height: auto; background: #161b22; border: 1px solid #21262d; border-radius: 6px; }}text {{ fill: #8b949e; font-size: 10px; }} table {{ border-collapse: collapse; width: 100%; margin-top: 1rem; }}th, td {{ border: 1px solid #21262d; padding: .3rem .6rem; text-align: left; }} th {{ color: #8b949e; }}ul {{ list-style: none; padding: 0; }} footer {{ color: #484f58; margin-top: 3rem; }}
</style></head><body><h1>\u{1f3c1} llmrace ramp</h1><p>throughput by concurrency — peak marks each provider's saturation point (by goodput; \u{2b07} flags &gt;10% post-peak drop)</p>{svg}<ul>{legend}</ul>{table}{ttfb_table}<footer>generated with <a style='color:#3390ff' href='https://github.com/dfbustosus/llmrace'>llmrace</a> v{} \u{1f980}</footer></body></html>",
        env!("CARGO_PKG_VERSION"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use llmrace::bench::{FailureClass, RequestAttempt, RequestOutcome, RoundFailure, RunMetrics};

    fn make_lane(concurrency: usize, tokens: u64, failed_rounds: usize) -> LaneSummary {
        LaneSummary {
            name: "mock".into(),
            model: "m1".into(),
            runs: vec![RunMetrics {
                total_ms: 1000.0,
                ttfb_ms: 0.0,
                tokens,
                ..RunMetrics::default()
            }],
            failures: (0..failed_rounds)
                .map(|_| RoundFailure {
                    task_checked: false,
                    requests: Vec::new(),
                    elapsed_ms: 1000.0,
                    class: FailureClass::Server,
                    attempts: 1,
                    message: "503".into(),
                    prompt_id: String::new(),
                })
                .collect(),
            concurrency,
            ..LaneSummary::default()
        }
    }

    fn make_lane_with_request_ttfb(
        name: &str,
        concurrency: usize,
        tokens: u64,
        ttfbs: &[f64],
    ) -> LaneSummary {
        let requests = ttfbs
            .iter()
            .map(|&ttfb_ms| RequestOutcome {
                elapsed_ms: 1000.0,
                attempts: vec![RequestAttempt::Success(RunMetrics {
                    total_ms: 1000.0,
                    ttfb_ms,
                    tokens: tokens / ttfbs.len().max(1) as u64,
                    ..RunMetrics::default()
                })],
            })
            .collect();
        LaneSummary {
            name: name.into(),
            model: "m1".into(),
            runs: vec![RunMetrics {
                requests,
                total_ms: 1000.0,
                ttfb_ms: ttfbs.first().copied().unwrap_or(0.0),
                tokens,
                ..RunMetrics::default()
            }],
            concurrency,
            ..LaneSummary::default()
        }
    }

    fn make_failed_lane(name: &str, concurrency: usize) -> LaneSummary {
        LaneSummary {
            name: name.into(),
            model: "m1".into(),
            runs: Vec::new(),
            failures: vec![RoundFailure {
                task_checked: false,
                requests: Vec::new(),
                elapsed_ms: 1000.0,
                class: FailureClass::Server,
                attempts: 1,
                message: "503".into(),
                prompt_id: String::new(),
            }],
            error: Some("503".into()),
            concurrency,
            ..LaneSummary::default()
        }
    }

    #[test]
    fn ramp_rows_and_html_use_goodput_peak_and_degradation_flag() {
        // c=1: 100 tok/s, 0 failures => goodput 100.0
        // c=4: 120 tok/s, 1 failure (50% success) => goodput 60.0 (degraded >10% below peak!)
        let phases = vec![
            RampLevel {
                concurrency: 1,
                summaries: vec![make_lane(1, 100, 0)],
            },
            RampLevel {
                concurrency: 4,
                summaries: vec![make_lane(4, 120, 1)],
            },
        ];
        let rows = build_ramp_rows(&phases);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].peak_concurrency, 1);
        assert!((rows[0].peak_goodput - 100.0).abs() < 0.1);
        assert!(rows[0].degraded);
        assert_eq!(rows[0].failure_rate_crosses_at, Some(4));

        let html = ramp_html(&[1, 4], &rows);
        assert!(html.contains("<b>c=1 (100.0) \u{2b07}</b>"), "{html}");
    }

    #[test]
    fn ramp_points_collect_request_ttfb_percentiles_and_json_fields() {
        // [10, 20, 30, 40, 100] -> p50 = 30.0, p95 = 88.0 (linear interpolation).
        let phases = vec![
            RampLevel {
                concurrency: 1,
                summaries: vec![make_lane_with_request_ttfb(
                    "mock",
                    1,
                    100,
                    &[10.0, 20.0, 30.0, 40.0, 100.0],
                )],
            },
            RampLevel {
                concurrency: 4,
                summaries: vec![make_failed_lane("mock", 4)],
            },
        ];
        let rows = build_ramp_rows(&phases);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].points.len(), 2);

        let p0 = &rows[0].points[0];
        assert_eq!(p0.concurrency, 1);
        let p50 = p0.request_ttfb_p50_ms.expect("p50 present at c=1");
        let p95 = p0.request_ttfb_p95_ms.expect("p95 present at c=1");
        assert!((p50 - 30.0).abs() < 1e-6, "got {p50}");
        assert!((p95 - 88.0).abs() < 1e-6, "got {p95}");

        let p1 = &rows[0].points[1];
        assert_eq!(p1.concurrency, 4);
        assert!(p1.request_ttfb_p50_ms.is_none());
        assert!(p1.request_ttfb_p95_ms.is_none());

        let json = build_ramp_json(&rows);
        assert_eq!(json.len(), 1);
        let entry = &json[0];
        assert_eq!(entry["request_ttfb_p50_ms_by_concurrency"]["1"], 30.0);
        assert!(entry["request_ttfb_p50_ms_by_concurrency"]["4"].is_null());
        assert_eq!(entry["request_ttfb_p95_ms_by_concurrency"]["1"], 88.0);
        assert!(entry["request_ttfb_p95_ms_by_concurrency"]["4"].is_null());
    }

    #[test]
    fn ramp_html_escapes_sub_millisecond_ttfb_and_handles_missing() {
        let phases = vec![
            RampLevel {
                concurrency: 1,
                summaries: vec![make_lane_with_request_ttfb("edge<1>", 1, 100, &[0.4])],
            },
            RampLevel {
                concurrency: 4,
                summaries: vec![make_failed_lane("edge<1>", 4)],
            },
        ];
        let rows = build_ramp_rows(&phases);
        let html = ramp_html(&[1, 4], &rows);
        assert!(html.contains("<h2>Request TTFT p95 (ms)</h2>"), "{html}");
        assert!(html.contains("<td>&lt;1</td>"), "{html}");
        assert!(!html.contains("<td><1</td>"), "raw '<1' in HTML: {html}");
        assert!(html.contains("<td>\u{2014}</td>"), "{html}");
        assert!(html.contains("edge&lt;1&gt;"), "{html}");
    }

    #[test]
    fn ramp_markdown_and_terminal_render_ttfb_table_and_sanitize_labels() {
        let phases = vec![
            RampLevel {
                concurrency: 1,
                summaries: vec![make_lane_with_request_ttfb(
                    "acme|edge\x1b[31m",
                    1,
                    100,
                    &[0.4],
                )],
            },
            RampLevel {
                concurrency: 4,
                summaries: vec![make_failed_lane("acme|edge\x1b[31m", 4)],
            },
        ];
        let rows = build_ramp_rows(&phases);

        let md = ramp_markdown_curve(&[1, 4], &rows);
        assert!(md.contains("### Request TTFT p95 (ms)"), "{md}");
        assert!(
            md.contains("| acme\\|edge[31m | &lt;1 | \u{2014} |"),
            "{md}"
        );
        assert!(!md.contains('\x1b'), "{md}");

        let term = ramp_terminal(&[1, 4], &rows);
        assert!(term.contains("Request TTFT p95 (ms)"), "{term}");
        assert!(term.contains("<1"), "{term}");
        assert!(term.contains('\u{2014}'), "{term}");
        assert!(!term.contains('\x1b'), "{term}");

        // When no concurrency level has TTFT p95 data, omit the secondary table.
        let empty_phases = vec![RampLevel {
            concurrency: 1,
            summaries: vec![make_failed_lane("mock", 1)],
        }];
        let empty_rows = build_ramp_rows(&empty_phases);
        assert!(!ramp_terminal(&[1], &empty_rows).contains("Request TTFT p95 (ms)"));
        assert!(!ramp_markdown_curve(&[1], &empty_rows).contains("Request TTFT p95 (ms)"));
        assert!(!ramp_html(&[1], &empty_rows).contains("Request TTFT p95 (ms)"));
    }
}
