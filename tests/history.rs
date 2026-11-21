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
        itl_p95_ms: None,
        reasoning_tokens: None,
        thinking_ms_estimate: None,
        failure_rate: None,
        goodput_tokens_per_sec: None,
        retry_attempts: 0,
        cost_per_run_usd: None,
        task_pass_rate: None,
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
    assert_eq!(record.failure_rate, Some(0.0));
    assert!(record.goodput_tokens_per_sec.unwrap() > 0.0);
    assert_eq!(record.retry_attempts, 0);
}

#[test]
fn old_history_without_goodput_still_loads() {
    let raw = r#"{"timestamp_unix":1,"provider":"a","model":"m","concurrency":1,"mean_total_ms":1.0,"p95_total_ms":1.0,"mean_ttfb_ms":1.0,"mean_tokens_per_sec":1.0}"#;
    let record: HistoryRecord = serde_json::from_str(raw).unwrap();
    assert!(record.failure_rate.is_none());
    assert!(record.goodput_tokens_per_sec.is_none());
    assert_eq!(record.retry_attempts, 0);
    assert!(record.cost_per_run_usd.is_none());
    assert!(record.task_pass_rate.is_none());
}

#[test]
fn trend_renders_itl_when_recorded() {
    let base = 1_700_000_000u64;
    let records: Vec<llmrace::history::HistoryRecord> = (0..3)
        .map(|i| {
            let mut r = rec(base + i * 3_600, "mock", "m1", 10.0, 50.0, 100.0);
            r.itl_p95_ms = Some(15.0 + i as f64);
            r
        })
        .collect();
    let trends = llmrace::history::build_trends(&records, 30, base + 7_200, &[]);
    assert_eq!(trends.len(), 1);
    assert!(trends[0].itl_p95_median.is_some());
    let table = llmrace::history::render_trend_table(&trends, 30);
    assert!(table.contains("ITLp95"), "{table}");
    assert!(table.contains("16ms"), "{table}");

    // records without ITL show the em dash
    let plain: Vec<llmrace::history::HistoryRecord> = (0..3)
        .map(|i| rec(base + i * 3_600, "mock", "m1", 10.0, 50.0, 100.0))
        .collect();
    let plain_trends = llmrace::history::build_trends(&plain, 30, base + 7_200, &[]);
    let plain_table = llmrace::history::render_trend_table(&plain_trends, 30);
    assert!(plain_table.contains("ITLp95"), "{plain_table}");
    assert!(
        !plain_table.contains("ms\n".to_string().trim()),
        "no itl value expected"
    );
}
