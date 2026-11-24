use llmrace::bench::{LaneSummary, RunMetrics};
use llmrace::gates::{render_ttfb_verdict, ttfb_offenders, Verdict};

fn lane(name: &str, ttfb: f64, successful: bool) -> LaneSummary {
    LaneSummary {
        name: name.into(),
        model: "m".into(),
        runs: if successful {
            vec![RunMetrics {
                total_ms: ttfb + 900.0,
                ttfb_ms: ttfb,
                tokens: 10,
                ..RunMetrics::default()
            }]
        } else {
            Vec::new()
        },
        failures: Vec::new(),
        retry_attempts: 0,
        error: if successful {
            None
        } else {
            Some("boom".into())
        },
        prompt: String::new(),
        concurrency: 1,
        variant: None,
        tool_task: None,
        price_book: None,
    }
}

#[test]
fn ttfb_offenders_filters_failed_and_under_threshold() {
    let lanes = vec![
        lane("fast", 100.0, true),
        lane("slow", 900.0, true),
        lane("dead", 5_000.0, false), // failed lanes never gate
    ];
    let offenders = ttfb_offenders(&lanes, 500.0);
    assert_eq!(offenders.len(), 1);
    assert_eq!(offenders[0].name, "slow");
    assert!((offenders[0].ttfb_ms - 900.0).abs() < 1e-9);
}

#[test]
fn ttfb_verdict_messages() {
    let pass = render_ttfb_verdict(&[], 500.0, false);
    assert_eq!(
        pass,
        Verdict::Pass("✓ TTFB gate passed (all providers under 500 ms)".into())
    );
    assert!(!pass.failed());

    let quiet = render_ttfb_verdict(&[], 500.0, true);
    assert_eq!(quiet.message(), "");

    let offenders = ttfb_offenders(&[lane("slow", 900.0, true)], 500.0);
    let fail = render_ttfb_verdict(&offenders, 500.0, false);
    assert!(fail.failed());
    assert!(fail.message().contains("slow"), "{}", fail.message());
}

#[test]
fn all_lanes_failed_detects_total_outage() {
    use llmrace::gates::all_lanes_failed;
    assert!(!all_lanes_failed(&[]), "no lanes raced is not a failure");
    let dead = vec![lane("a", 1.0, false), lane("b", 1.0, false)];
    assert!(all_lanes_failed(&dead));
    let mixed = vec![lane("a", 1.0, false), lane("b", 10.0, true)];
    assert!(!all_lanes_failed(&mixed));
}

#[test]
fn significance_gate_separates_noise_from_real_regressions() {
    // Baseline and current with per-run data: means breach the threshold
    // but with heavy overlap (noise).
    let noisy = r#"{"providers":[{
        "provider":"mock","mean_total_ms":200.0,"p95_total_ms":220.0,
        "mean_ttfb_ms":20.0,"mean_tokens_per_sec":80.0,
        "runs":[
          {"total_ms":150.0,"ttfb_ms":20.0,"tokens_per_sec":80.0},
          {"total_ms":250.0,"ttfb_ms":20.0,"tokens_per_sec":80.0},
          {"total_ms":180.0,"ttfb_ms":20.0,"tokens_per_sec":80.0},
          {"total_ms":230.0,"ttfb_ms":20.0,"tokens_per_sec":80.0}]}]}"#;
    let current_noisy = r#"{"providers":[{
        "provider":"mock","mean_total_ms":260.0,"p95_total_ms":280.0,
        "mean_ttfb_ms":20.0,"mean_tokens_per_sec":79.0,
        "runs":[
          {"total_ms":250.0,"ttfb_ms":20.0,"tokens_per_sec":79.0},
          {"total_ms":190.0,"ttfb_ms":20.0,"tokens_per_sec":79.0},
          {"total_ms":300.0,"ttfb_ms":20.0,"tokens_per_sec":79.0},
          {"total_ms":210.0,"ttfb_ms":20.0,"tokens_per_sec":79.0}]}]}"#;
    let base = llmrace::regression::BaselineReport::from_json_str(noisy).unwrap();
    let cur = llmrace::regression::BaselineReport::from_json_str(current_noisy).unwrap();
    let report = llmrace::regression::compare_baselines(&cur, &base, 20.0);
    assert!(
        report.any_regressed(),
        "threshold breached: mean 200 -> 260 is +30%"
    );
    assert!(
        !report.any_regressed_significant(),
        "overlapping samples must not count as significant"
    );
    // p-values present and rendered
    let table = report.terminal_table(20.0);
    assert!(table.contains("p="), "{table}");

    // a clear regression stays significant
    let clear_current = r#"{"providers":[{
        "provider":"mock","mean_total_ms":400.0,"p95_total_ms":420.0,
        "mean_ttfb_ms":20.0,"mean_tokens_per_sec":79.0,
        "runs":[
          {"total_ms":395.0,"ttfb_ms":20.0,"tokens_per_sec":79.0},
          {"total_ms":405.0,"ttfb_ms":20.0,"tokens_per_sec":79.0},
          {"total_ms":400.0,"ttfb_ms":20.0,"tokens_per_sec":79.0}]}]}"#;
    let cur2 = llmrace::regression::BaselineReport::from_json_str(clear_current).unwrap();
    let report2 = llmrace::regression::compare_baselines(&cur2, &base, 20.0);
    assert!(
        report2.any_regressed_significant(),
        "clean 200 -> 400 shift is real"
    );
}
