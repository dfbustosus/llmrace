use llmrace::dashboard::render_html;
use llmrace::history::HistoryRecord;

fn rec(ts: u64, provider: &str, model: &str, ttfb: f64, tps: f64) -> HistoryRecord {
    HistoryRecord {
        timestamp_unix: ts,
        provider: provider.into(),
        model: model.into(),
        variant: None,
        concurrency: 1,
        mean_total_ms: ttfb * 8.0,
        p95_total_ms: ttfb * 9.0,
        mean_ttfb_ms: ttfb,
        mean_tokens_per_sec: tps,
    }
}

#[test]
fn dashboard_renders_charts_models_and_tables() {
    let now = 1_700_000_000u64;
    let day = 86_400u64;
    let records = vec![
        rec(now - 10 * day, "groq", "llama-3", 20.0, 400.0),
        rec(now - 5 * day, "groq", "llama-3", 30.0, 380.0),
        rec(now - 1 * day, "groq", "llama-4", 25.0, 410.0),
        rec(now - 2 * day, "openai", "gpt-4o-mini", 200.0, 80.0),
        rec(now - 1 * day, "openai", "gpt-4o-mini", 210.0, 78.0),
    ];

    let html = render_html(&records, 30, now);
    assert!(html.contains("<!DOCTYPE html>"));
    assert!(html.contains("<h2>groq"));
    assert!(html.contains("llama-3 → llama-4"), "model swap annotation");
    assert!(html.contains("<polyline class='line'"));
    assert!(html.contains("TTFB (ms)"));
    assert!(html.contains("tokens/s"));
    assert!(html.contains("▼ lower is better"));
    assert!(html.contains("▲ higher is better"));
    assert!(html.contains("<table>"));
    assert!(html.contains("no external") == false);
    assert!(!html.contains("http://cdn") && !html.contains("https://cdn"));
}

#[test]
fn dashboard_empty_and_window_filtering() {
    let now = 1_700_000_000u64;
    let empty = render_html(&[], 30, now);
    assert!(empty.contains("no history in the window"));

    let old_only = vec![rec(now - 90 * 86_400, "groq", "m", 10.0, 10.0)];
    let html = render_html(&old_only, 30, now);
    assert!(
        !html.contains("<h2>groq"),
        "outside window must be excluded"
    );

    let single = vec![rec(now - 100, "groq", "m", 10.0, 10.0)];
    let html = render_html(&single, 30, now);
    assert!(html.contains("at least two recorded runs"));
}
