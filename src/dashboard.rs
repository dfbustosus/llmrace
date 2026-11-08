//! Static HTML dashboard rendered from recorded history.
//!
//! `llmrace --dashboard --out site/index.html` produces a fully
//! self-contained dark-theme page with per-provider SVG time series — no
//! server, no JavaScript, no external assets.

use crate::history::HistoryRecord;
use crate::stats::Sample;

const WIDTH: f64 = 720.0;
const HEIGHT: f64 = 150.0;
const PAD: f64 = 28.0;

/// Renders the dashboard HTML for records within the last `days` days.
pub fn render_html(records: &[HistoryRecord], days: u64, now_unix: u64) -> String {
    let cutoff = now_unix.saturating_sub(days * 86_400);
    let window: Vec<&HistoryRecord> = records
        .iter()
        .filter(|r| r.timestamp_unix >= cutoff)
        .collect();

    let mut providers: Vec<&str> = Vec::new();
    for r in &window {
        if !providers.contains(&r.provider.as_str()) {
            providers.push(&r.provider);
        }
    }

    let mut body = String::new();
    if providers.is_empty() {
        body.push_str("<p class='empty'>no history in the window — run <code>llmrace ... --record</code> to start recording</p>");
    }
    for provider in &providers {
        let lanes: Vec<&&HistoryRecord> =
            window.iter().filter(|r| r.provider == *provider).collect();
        let models = distinct_models(&lanes);
        body.push_str(&format!(
            "<section><h2>{provider} <span class='model'>{}</span></h2>",
            models.join(" → ")
        ));
        if lanes.len() >= 2 {
            body.push_str(&chart("TTFB (ms)", &lanes, |r| r.mean_ttfb_ms, true));
            body.push_str(&chart("tokens/s", &lanes, |r| r.mean_tokens_per_sec, false));
        } else {
            body.push_str("<p class='empty'>needs at least two recorded runs for a chart</p>");
        }
        if let Some(last) = lanes.last() {
            body.push_str(&format!(
                "<table><tr><th>run</th><th>model</th><th>TTFB</th><th>tok/s</th><th>p95</th></tr><tr><td>{}</td><td>{}</td><td>{:.0} ms</td><td>{:.1}</td><td>{:.0} ms</td></tr></table>",
                date(last.timestamp_unix),
                last.model,
                last.mean_ttfb_ms,
                last.mean_tokens_per_sec,
                last.p95_total_ms
            ));
        }
        body.push_str("</section>");
    }

    let generated = date(now_unix);
    format!(
        "<!DOCTYPE html><html lang='en'><head><meta charset='utf-8'><meta name='viewport' content='width=device-width,initial-scale=1'>\
<title>llmrace dashboard</title><style>\
:root {{ color-scheme: dark; }}\
body {{ font-family: ui-monospace, 'SF Mono', Menlo, monospace; background: #0d1117; color: #e6edf3; margin: 0 auto; max-width: 780px; padding: 2rem 1rem; }}\
h1 {{ color: #3390ff; }} h2 {{ border-bottom: 1px solid #21262d; padding-bottom: .3rem; }}\
h2 .model {{ color: #8b949e; font-size: .7em; font-weight: normal; }}\
section {{ margin: 2rem 0; }} svg {{ width: 100%; height: auto; background: #161b22; border: 1px solid #21262d; border-radius: 6px; }}\
text {{ fill: #8b949e; font-size: 10px; }} .line {{ fill: none; stroke: #3390ff; stroke-width: 2; }} .dot {{ fill: #3390ff; }}\
table {{ border-collapse: collapse; margin-top: .6rem; width: 100%; }} th, td {{ border: 1px solid #21262d; padding: .3rem .6rem; text-align: left; }}\
th {{ color: #8b949e; }} .empty {{ color: #8b949e; }} footer {{ color: #484f58; margin-top: 3rem; }}\
</style></head><body>\
<h1>🏁 llmrace dashboard</h1>\
<p class='empty'>last {days} days · generated {generated}</p>\
{body}\
<footer>generated with <a style='color:#3390ff' href='https://github.com/dfbustosus/llmrace'>llmrace</a> 🦀</footer>\
</body></html>"
    )
}

fn chart(
    title: &str,
    lanes: &[&&HistoryRecord],
    value: impl Fn(&HistoryRecord) -> f64,
    lower_is_better: bool,
) -> String {
    let points: Vec<(u64, f64)> = lanes.iter().map(|r| (r.timestamp_unix, value(r))).collect();
    let sample = Sample::from_values(points.iter().map(|(_, v)| *v).collect());
    let (min_v, max_v) = match (sample.min(), sample.max()) {
        (Some(a), Some(b)) => (a, b),
        _ => return String::new(),
    };
    let median = sample.median();
    let span_v = (max_v - min_v).max(1e-9);
    let (min_t, max_t) = (
        points.iter().map(|(t, _)| *t).min().unwrap_or(0),
        points.iter().map(|(t, _)| *t).max().unwrap_or(1),
    );
    let span_t = (max_t - min_t).max(1);

    let coords: Vec<String> = points
        .iter()
        .map(|(t, v)| {
            let x = PAD + (t - min_t) as f64 / span_t as f64 * (WIDTH - 2.0 * PAD);
            let y = HEIGHT - PAD - (v - min_v) / span_v * (HEIGHT - 2.0 * PAD);
            format!("{x:.1},{y:.1}")
        })
        .collect();
    let dots: Vec<String> = coords
        .iter()
        .map(|c| {
            let (x, y) = c.split_once(',').unwrap_or((c, "0"));
            format!("<circle class='dot' cx='{x}' cy='{y}' r='3'/>")
        })
        .collect();
    let median_y = HEIGHT - PAD - (median - min_v) / span_v * (HEIGHT - 2.0 * PAD);
    let unit = if lower_is_better {
        "▼ lower is better"
    } else {
        "▲ higher is better"
    };

    format!(
        "<h3>{title} <span class='empty'>{unit}</span></h3>\
<svg viewBox='0 0 {WIDTH} {HEIGHT}' xmlns='http://www.w3.org/2000/svg' role='img' aria-label='{title}'>\
<line x1='{PAD}' y1='{median_y:.1}' x2='{right:.1}' y2='{median_y:.1}' stroke='#30363d' stroke-dasharray='4 3'/>\
<polyline class='line' points='{}'/>{}\
<text x='{PAD}' y='14'>{max_v:.1} (median {median:.1})</text>\
<text x='{right:.1}' y='{bottom:.1}'>{min_v:.1}</text>\
<text x='{PAD}' y='{HEIGHT:.0}'>{}</text>\
<text x='{right:.1}' y='{HEIGHT:.0}'>{}</text>\
</svg>",
        coords.join(" "),
        dots.join(""),
        date(min_t),
        date(max_t),
        right = WIDTH - PAD,
        bottom = HEIGHT - 8.0,
    )
}

fn distinct_models(lanes: &[&&HistoryRecord]) -> Vec<String> {
    let mut models: Vec<String> = Vec::new();
    for r in lanes {
        if !models.contains(&r.model) {
            models.push(r.model.clone());
        }
    }
    models
}

fn date(unix: u64) -> String {
    let days = unix / 86_400;
    let (y, m, d) = civil_from_days(days as i64);
    format!("{y:04}-{m:02}-{d:02}")
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
