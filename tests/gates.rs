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
        error: if successful {
            None
        } else {
            Some("boom".into())
        },
        prompt: String::new(),
        concurrency: 1,
        variant: None,
        tool_task: None,
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
