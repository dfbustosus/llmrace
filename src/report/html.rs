//! Self-contained interactive dark-theme HTML report rendering.

use super::{rank, sampling_inline};
use crate::bench::LaneSummary;
use crate::stats::fmt_ms;

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
    html_report_with_sampling(summaries, prompt, runs, max_tokens, groups, None)
}

/// Renders an interactive HTML report with grouped podiums and an optional
/// inline sampling summary in the metadata header.
#[must_use]
pub fn html_report_with_sampling(
    summaries: &[LaneSummary],
    prompt: &str,
    runs: usize,
    max_tokens: u64,
    groups: &[(Option<String>, Vec<crate::score::Scored>)],
    sampling: Option<&crate::provider::Sampling>,
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

    body.push_str("<section><h2>Results</h2><table><tr><th>#</th><th>Provider</th><th>Model</th><th>C</th><th>Latency (mean ± 95% CI)</th><th>p95</th><th>TTFT</th><th>ITL p95</th><th>Pass</th><th>Fail</th><th>Tokens/s</th></tr>");
    for (i, s) in rank(summaries).iter().enumerate() {
        let ci = match s.ci95_total_ms() {
            Some((lo, hi)) => format!("±{:.0}", (hi - lo) / 2.0),
            None => "—".to_string(),
        };
        body.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{:.0} ms {}</td><td>{:.0} ms</td><td>{}</td><td>{}</td><td>{}</td><td>{}/{}</td><td><b>{:.1}</b></td></tr>",
            i + 1,
            crate::util::html_escape(&s.name),
            crate::util::html_escape(&s.model),
            s.concurrency,
            s.mean_total_ms(),
            ci,
            s.p95_total_ms(),
            crate::util::html_escape(&fmt_ms(s.mean_ttfb_ms())),
            s.itl_p95_ms().map(|v| format!("{v:.0} ms")).unwrap_or_else(|| "—".into()),
            match s.task_pass() {
                Some((ok, n)) => format!("{ok}/{n}"),
                None => "—".to_string(),
            },
            s.failed_rounds(),
            s.measured_rounds(),
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
    let sampling_segment = sampling
        .and_then(sampling_inline)
        .map(|inline| format!("{} · ", crate::util::html_escape(&inline)))
        .unwrap_or_default();
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
<p class='empty'>{} runs · max {} tokens · prompt: <code>{}{}</code> · {}{}</p>\
{body}\
<footer>generated with <a style='color:#3390ff' href='https://github.com/dfbustosus/llmrace'>llmrace</a> v{} 🦀</footer>\
</body></html>",
        runs,
        max_tokens,
        crate::util::html_escape(&label),
        if prompt.chars().count() > 120 { "…" } else { "" },
        sampling_segment,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bench::RunMetrics;
    use crate::provider::{JsonOutput, Sampling};

    #[test]
    fn html_report_escapes_sub_millisecond_ttfb() {
        let fast = LaneSummary {
            name: "local".into(),
            model: "m".into(),
            concurrency: 1,
            runs: vec![RunMetrics {
                total_ms: 10.0,
                ttfb_ms: 0.4,
                tokens: 5,
                ..RunMetrics::default()
            }],
            ..LaneSummary::default()
        };
        let html = html_report(&[fast], "hi", 1, 32, &[]);
        assert!(html.contains("<td>&lt;1</td>"), "{html}");
        assert!(!html.contains("<td><1</td>"), "{html}");
    }

    #[test]
    fn html_report_with_sampling_renders_inline_and_escapes_html() {
        let lane = LaneSummary {
            name: "p<1>".into(),
            model: "m&2".into(),
            concurrency: 1,
            runs: vec![RunMetrics {
                total_ms: 100.0,
                ttfb_ms: 20.0,
                tokens: 10,
                ..RunMetrics::default()
            }],
            ..LaneSummary::default()
        };
        let sampling = Sampling {
            temperature: Some(0.0),
            top_p: Some(0.9),
            seed: Some(7),
            system: Some("sys <tag>".into()),
            json_output: Some(JsonOutput::Object),
        };
        let prompt = "compare <code> · <script>";
        let html = html_report_with_sampling(
            std::slice::from_ref(&lane),
            prompt,
            2,
            64,
            &[],
            Some(&sampling),
        );
        let expected_meta = format!(
            "prompt: <code>compare &lt;code&gt; · &lt;script&gt;</code> · temperature 0, top_p 0.9, seed 7, system prompt, json output · {}",
            crate::util::today_utc()
        );
        assert!(html.contains(&expected_meta), "{html}");
        assert!(!html.contains("<script>"), "{html}");
        assert!(html.contains("p&lt;1&gt;"), "{html}");
        assert!(html.contains("m&amp;2"), "{html}");

        // Unset sampling must produce the exact same output as `None` / `html_report_grouped`.
        let unset = Sampling::default();
        let html_unset = html_report_with_sampling(
            std::slice::from_ref(&lane),
            prompt,
            2,
            64,
            &[],
            Some(&unset),
        );
        let html_none = html_report_grouped(&[lane], prompt, 2, 64, &[]);
        assert_eq!(html_unset, html_none);
    }
}
