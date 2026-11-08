use llmrace::bench::BenchConfig;
use llmrace::provider::ProviderSpec;
use llmrace::watch::{degrade_check, watch, WatchConfig};

#[test]
fn degrade_check_fires_only_beyond_threshold() {
    let hist = vec![100.0, 110.0, 120.0];
    assert!(degrade_check(1, "p", "ttfb", &hist, 125.0, 15.0).is_none());
    let alert = degrade_check(1, "p", "ttfb", &hist, 160.0, 15.0).unwrap();
    assert_eq!(alert.provider, "p");
    assert_eq!(alert.metric, "ttfb");
    assert_eq!(alert.baseline, 110.0);
    assert!(alert.delta_pct > 45.0 && alert.delta_pct < 46.0);
    assert!(degrade_check(1, "p", "ttfb", &[], 999.0, 15.0).is_none());
    assert!(degrade_check(1, "p", "ttfb", &[0.0, 0.0], 5.0, 15.0).is_none());
}

#[tokio::test]
async fn watch_collects_rounds_and_stops_at_limit() {
    let config = WatchConfig {
        every_secs: 0,
        max_regression_pct: 1000.0,
        rounds: Some(2),
        bench: BenchConfig {
            prompt: "demo".into(),
            runs: 1,
            warmup: 0,
            ..BenchConfig::default()
        },
    };
    let rounds_seen = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = std::sync::Arc::clone(&rounds_seen);
    let report = watch(
        vec![ProviderSpec::mock()],
        "mock-1".into(),
        config,
        reqwest::Client::new(),
        move |round, summaries, alerts| {
            counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            assert_eq!(round, counter.load(std::sync::atomic::Ordering::Relaxed));
            assert_eq!(summaries.len(), 1);
            assert!(alerts.is_empty(), "huge threshold => no alerts");
        },
        || false,
    )
    .await;
    assert_eq!(report.rounds.len(), 2);
    assert_eq!(rounds_seen.load(std::sync::atomic::Ordering::Relaxed), 2);
    assert!(!report.last_round().is_empty());
    assert!(report.last_round()[0].successful());
}

#[tokio::test]
async fn watch_stop_flag_ends_before_first_round() {
    let config = WatchConfig {
        every_secs: 0,
        max_regression_pct: 15.0,
        rounds: None,
        bench: BenchConfig::default(),
    };
    let report = watch(
        vec![ProviderSpec::mock()],
        "mock-1".into(),
        config,
        reqwest::Client::new(),
        |_, _, _| {},
        || true,
    )
    .await;
    assert!(report.rounds.is_empty());
    assert!(report.last_round().is_empty());
}
