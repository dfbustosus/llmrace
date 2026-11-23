//! Saturation-curve mode: race at each concurrency level.

use super::collect::race_collect;
use super::config::RunConfig;
use anyhow::{Context, Result};
use llmrace::bench::LaneSummary;
use std::fmt::Write as _;

/// One `--ramp` level and its lane summaries.
pub(crate) struct RampLevel {
    /// Concurrency raced at this level.
    pub concurrency: usize,
    /// Lane results at this level.
    pub summaries: Vec<LaneSummary>,
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
pub(crate) async fn ramp_race(cfg: RunConfig, levels: &[usize]) -> Result<i32> {
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
    // graceful interrupt across phases: SIGINT finishes the in-flight
    // phase's rounds and skips the remaining levels
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let stop = std::sync::Arc::clone(&stop);
        tokio::spawn(async move {
            let _ = tokio::signal::ctrl_c().await;
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
            eprintln!("\n\u{23f9} interrupt received — finishing in-flight rounds, writing the curve so far…");
        });
    }
    for (idx, level) in levels.iter().enumerate() {
        if stop.load(std::sync::atomic::Ordering::Relaxed) && idx > 0 {
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
        let summaries = race_collect(&cfg, Some(bench), true, std::sync::Arc::clone(&stop)).await?;
        phases.push(RampLevel {
            concurrency: *level,
            summaries,
        });
    }

    // curve table (terminal + markdown)
    let mut curve = String::from("| Provider |");
    let mut header = String::new();
    for level in levels {
        let _ = write!(header, " c={level} |");
    }
    curve.push_str(&header);
    curve.push_str(" peak |\n|---|");
    for _ in levels {
        curve.push_str("---:|");
    }
    curve.push_str("---:|\n");

    let mut curve_term = format!("{:<20}", "Provider");
    for level in levels {
        let _ = write!(curve_term, " {:>10}", format!("c={level}"));
    }
    curve_term.push_str("      peak\n");
    curve_term.push_str(&"-".repeat(curve_term.len()));
    curve_term.push('\n');

    // stable lane order from the first phase
    let order: Vec<(String, String)> = phases
        .first()
        .map(|p| {
            p.summaries
                .iter()
                .map(|s| (s.name.clone(), s.model.clone()))
                .collect()
        })
        .unwrap_or_default();

    let mut ramp_json: Vec<serde_json::Value> = Vec::new();
    for (name, model) in &order {
        let mut md_row = format!("| {name} |");
        let mut term_row = format!("{:<20}", name);
        let mut best: Option<(usize, f64)> = None;
        let mut per_level = serde_json::Map::new();
        for phase in &phases {
            let lane = phase
                .summaries
                .iter()
                .find(|s| s.name == *name && s.model == *model);
            let tps = lane
                .filter(|s| s.successful())
                .map(|s| s.mean_tokens_per_sec())
                .unwrap_or(0.0);
            let goodput = lane
                .filter(|s| s.successful())
                .map(|s| s.goodput_tokens_per_sec())
                .unwrap_or(0.0);
            if best.is_none_or(|(_, b)| goodput > b) {
                best = Some((phase.concurrency, goodput));
            }
            per_level.insert(
                phase.concurrency.to_string(),
                serde_json::json!((tps * 10.0).round() / 10.0),
            );
            let _ = write!(md_row, " {tps:.1} |");
            let _ = write!(term_row, " {tps:>10.1}");
        }
        let peak = best.unwrap_or((1, 0.0));
        let crosses = phases.iter().find(|ph| {
            ph.summaries
                .iter()
                .find(|s| s.name == *name && s.model == *model)
                .and_then(|s| s.failure_rate())
                .is_some_and(|rate| rate > 0.05)
        });
        // flag degradation: any later level >10% below peak
        let degraded = phases.iter().any(|ph| {
            ph.concurrency > peak.0
                && ph
                    .summaries
                    .iter()
                    .find(|s| s.name == *name && s.model == *model)
                    .filter(|s| s.successful())
                    .map(|s| s.goodput_tokens_per_sec())
                    .is_some_and(|goodput| goodput < peak.1 * 0.9)
        });
        let peak_tps = phases
            .iter()
            .find(|ph| ph.concurrency == peak.0)
            .and_then(|ph| {
                ph.summaries
                    .iter()
                    .find(|s| s.name == *name && s.model == *model)
            })
            .filter(|s| s.successful())
            .map(|s| s.mean_tokens_per_sec())
            .unwrap_or(0.0);
        let peak_note = if degraded {
            format!("c={} ({:.1}) \u{2b07}", peak.0, peak.1)
        } else {
            format!("c={} ({:.1})", peak.0, peak.1)
        };
        let _ = writeln!(md_row, " {peak_note} |");
        let _ = writeln!(term_row, "  {peak_note}");
        curve.push_str(&md_row);
        curve_term.push_str(&term_row);
        ramp_json.push(serde_json::json!({
            "provider": name,
            "model": model,
            "tokens_per_sec_by_concurrency": per_level,
            "peak_concurrency": peak.0,
            "peak_tokens_per_sec": (peak_tps * 10.0).round() / 10.0,
            "peak_goodput_tokens_per_sec": (peak.1 * 10.0).round() / 10.0,
            "failure_rate_crosses_at": crosses.map(|ph| ph.concurrency),
            "degrades_past_peak": degraded,
        }));
    }

    if !cfg.quiet {
        cfg.status(format!("\n{curve_term}"));
    }

    // reports: markdown curve section + structured json
    let mut md = String::from("# \u{1f3c1} llmrace ramp\n\n");
    md.push_str(&format!(
        "- **Prompt**: `\"{}\"`\n- **Levels**: {:?}\n- **Rounds per level**: {}\n",
        if cfg.bench.workload.is_empty() {
            cfg.prompt.replace('\n', " ")
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
    md.push_str("\n> Peak is the concurrency where goodput is highest. The cells are tokens/s of successful rounds. \u{2b07} flags a goodput drop of more than 10% past that peak. A failure rate above 5% is `failure_rate_crosses_at` in the JSON.\n");

    let out_ext = if cfg.out == "-" {
        None
    } else {
        std::path::Path::new(&cfg.out)
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
        std::fs::write(&md_path, &md).with_context(|| format!("writing {md_path}"))?;
        cfg.status(format!("\u{1f4c4} ramp report saved to {md_path}"));
    }

    if cfg.html || out_ext.as_deref() == Some("html") {
        let h = ramp_html(&order, &phases);
        let path = crate::report_sibling(&cfg.out, "html");
        std::fs::write(&path, h).with_context(|| format!("writing {path}"))?;
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
        std::fs::write(&path, serde_json::to_string_pretty(&payload)?)
            .with_context(|| format!("writing {path}"))?;
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

/// Self-contained HTML page for a ramp: SVG throughput-by-concurrency
/// series per provider plus the curve table.
fn ramp_html(order: &[(String, String)], phases: &[RampLevel]) -> String {
    const W: f64 = 760.0;
    const H: f64 = 320.0;
    const PAD: f64 = 44.0;
    const COLORS: [&str; 6] = [
        "#3390ff", "#3fb950", "#d29922", "#f778ba", "#a371f7", "#39c5cf",
    ];

    let levels: Vec<usize> = phases.iter().map(|p| p.concurrency).collect();
    let tps = |name: &str, model: &str, level: usize| -> f64 {
        phases
            .iter()
            .find(|p| p.concurrency == level)
            .and_then(|p| {
                p.summaries
                    .iter()
                    .find(|s| s.name == name && s.model == model)
            })
            .filter(|s| s.successful())
            .map(|s| s.mean_tokens_per_sec())
            .unwrap_or(0.0)
    };
    let max_tps = order
        .iter()
        .flat_map(|(n, m)| levels.iter().map(move |l| tps(n, m, *l)))
        .fold(1.0, f64::max);
    let max_level = *levels.last().unwrap_or(&1) as f64;
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
    for level in &levels {
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
    for (i, (name, model)) in order.iter().enumerate() {
        let color = COLORS[i % COLORS.len()];
        let points: Vec<String> = levels
            .iter()
            .map(|l| format!("{:.1},{:.1}", x(*l), y(tps(name, model, *l))))
            .collect();
        svg.push_str(&format!(
            "<polyline fill='none' stroke='{color}' stroke-width='2' points='{}'/>",
            points.join(" ")
        ));
        for l in &levels {
            svg.push_str(&format!(
                "<circle cx='{:.1}' cy='{:.1}' r='3' fill='{color}'/>",
                x(*l),
                y(tps(name, model, *l))
            ));
        }
        legend.push_str(&format!(
            "<li><span style='color:{color}'>\u{25cf}</span> {} ({})</li>",
            llmrace::util::html_escape(name),
            llmrace::util::html_escape(model)
        ));
    }
    svg.push_str("</svg>");

    // reuse the markdown curve table, converted loosely: rebuild as HTML
    let mut table = String::from("<table><tr><th>Provider</th>");
    for level in &levels {
        table.push_str(&format!("<th>c={level}</th>"));
    }
    table.push_str("<th>peak</th></tr>");
    for (name, model) in order {
        table.push_str(&format!(
            "<tr><td>{} ({})</td>",
            llmrace::util::html_escape(name),
            llmrace::util::html_escape(model)
        ));
        let mut peak = (1usize, 0.0);
        for level in &levels {
            let v = tps(name, model, *level);
            if v > peak.1 {
                peak = (*level, v);
            }
            table.push_str(&format!("<td>{v:.1}</td>"));
        }
        table.push_str(&format!(
            "<td><b>c={} ({:.1})</b></td></tr>",
            peak.0, peak.1
        ));
    }
    table.push_str("</table>");

    format!(
        "<!DOCTYPE html><html lang='en'><head><meta charset='utf-8'><meta name='viewport' content='width=device-width,initial-scale=1'><title>llmrace ramp</title><style>:root {{ color-scheme: dark; }}body {{ font-family: ui-monospace, 'SF Mono', Menlo, monospace; background: #0d1117; color: #e6edf3; margin: 0 auto; max-width: 820px; padding: 2rem 1rem; }}h1 {{ color: #3390ff; }} svg {{ width: 100%; height: auto; background: #161b22; border: 1px solid #21262d; border-radius: 6px; }}text {{ fill: #8b949e; font-size: 10px; }} table {{ border-collapse: collapse; width: 100%; margin-top: 1rem; }}th, td {{ border: 1px solid #21262d; padding: .3rem .6rem; text-align: left; }} th {{ color: #8b949e; }}ul {{ list-style: none; padding: 0; }} footer {{ color: #484f58; margin-top: 3rem; }}
</style></head><body><h1>\u{1f3c1} llmrace ramp</h1><p>throughput by concurrency — peak marks each provider's saturation point</p>{svg}<ul>{legend}</ul>{table}<footer>generated with <a style='color:#3390ff' href='https://github.com/dfbustosus/llmrace'>llmrace</a> v{} \u{1f980}</footer></body></html>",
        env!("CARGO_PKG_VERSION"),
    )
}
