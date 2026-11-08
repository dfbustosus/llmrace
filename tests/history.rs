use llmrace::bench::spawn_lane;
use llmrace::bench::{BenchConfig, LaneSummary};
use llmrace::history::{append, build_trends, load, record_from_summary, HistoryRecord};
use llmrace::provider::ProviderSpec;

fn rec(ts: u64, provider: &str, model: &str, ttfb: f64, tps: f64, p95: f64) -> HistoryRecord {
    HistoryRecord {
        timestamp_unix: ts,
        provider: provider.into(),
        model: model.into(),
        variant: None,
        concurrency: 1,
        mean_total_ms: p95 * 0.8,
        p95_total_ms: p95,
        mean_ttfb_ms: ttfb,
        mean_tokens_per_sec: tps,
    }
}

#[test]
fn append_and_load_roundtrip_with_corrupt_tolerance() {
    let dir = std::env::temp_dir().join(format!("llmrace-hist-{}", std::process::id()));
    let path = dir.join("history.jsonl");
    let _ = std::fs::remove_file(&path);
    append(&path, &[rec(1000, "a", "m1", 10.0, 50.0, 100.0)]).unwrap();
    append(&path, &[rec(2000, "a", "m1", 12.0, 48.0, 110.0)]).unwrap();
    std::fs::write(&path, {
        let existing = std::fs::read_to_string(&path).unwrap();
        format!("{existing}{{corrupt json\n\nnot json at all\n")
    })
    .unwrap();
    let loaded = load(&path);
    assert_eq!(loaded.len(), 2);
    assert_eq!(loaded[0].provider, "a");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn trends_window_filter_and_model_swaps() {
    let now = 1_700_000_000u64;
    let records = vec![
        rec(now - 40 * 86_400, "groq", "llama-3", 20.0, 400.0, 500.0),
        rec(now - 5 * 86_400, "groq", "llama-3", 30.0, 380.0, 600.0),
        rec(now - 86_400, "groq", "llama-4", 45.0, 300.0, 800.0),
        rec(
            now - 2 * 86_400,
            "openai",
            "gpt-4o-mini",
            200.0,
            80.0,
            900.0,
        ),
        rec(now - 86_400, "openai", "gpt-4o-mini", 210.0, 78.0, 950.0),
    ];

    let trends = build_trends(&records, 30, now, &[]);
    assert_eq!(trends.len(), 2);

    let groq = trends.iter().find(|t| t.name == "groq").unwrap();
    assert!(groq.model_swapped());
    assert_eq!(groq.models_seen, vec!["llama-3", "llama-4"]);
    assert_eq!(groq.current_model, "llama-4");
    assert!((groq.ttfb_delta_pct() - 20.0).abs() < 0.01);

    let openai = trends.iter().find(|t| t.name == "openai").unwrap();
    assert!(!openai.model_swapped());
    assert!((openai.ttfb_delta_pct() - 2.439).abs() < 0.01);

    let filtered = build_trends(&records, 30, now, &["groq".to_string()]);
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].name, "groq");

    let narrow = build_trends(&records, 2, now, &[]);
    assert!(narrow.iter().all(|t| t.name != "groq") || narrow.is_empty());
}

#[test]
fn provider_with_single_record_is_skipped() {
    let now = 1_700_000_000u64;
    let records = vec![rec(now, "solo", "m", 1.0, 1.0, 1.0)];
    assert!(build_trends(&records, 30, now, &[]).is_empty());
}

#[tokio::test]
async fn record_from_summary_maps_all_fields() {
    let config = BenchConfig {
        prompt: "p".into(),
        runs: 1,
        warmup: 0,
        concurrency: 2,
        ..BenchConfig::default()
    };
    let (handle, progress) = spawn_lane(
        reqwest::Client::new(),
        ProviderSpec::mock(),
        "mock-1".into(),
        config,
    );
    drop(progress);
    let summary: LaneSummary = handle.await.unwrap();
    let record = record_from_summary(&summary, 42);
    assert_eq!(record.timestamp_unix, 42);
    assert_eq!(record.provider, "mock");
    assert_eq!(record.concurrency, 2);
    assert!(record.mean_tokens_per_sec > 0.0);
    assert!(record.p95_total_ms >= record.mean_total_ms * 0.99);
}
