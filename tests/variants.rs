use llmrace::bench::{spawn_lane, BenchConfig, LaneSummary};
use llmrace::provider::ProviderSpec;
use llmrace::report;
use llmrace::variants;

#[tokio::test]
async fn sweep_runs_every_variant_for_every_provider() {
    let vs = variants::parse("short,json").unwrap();
    assert_eq!(vs.len(), 2);
    let mut summaries = Vec::new();
    for v in &vs {
        let config = BenchConfig {
            prompt: v.prompt.to_string(),
            runs: 1,
            warmup: 0,
            ..BenchConfig::default()
        };
        let (handle, progress) = spawn_lane(
            reqwest::Client::new(),
            ProviderSpec::mock(),
            "mock-1".into(),
            config,
        );
        drop(progress);
        let mut summary: LaneSummary = handle.await.unwrap();
        summary.variant = Some(v.name.to_string());
        assert!(summary.successful());
        summaries.push(summary);
    }

    let md = report::markdown(&summaries, "sweep", 1, 32);
    assert!(md.contains("variant: `short`"));
    assert!(md.contains("variant: `json`"));
    assert!(md.contains("structured output decoding"));

    let json = report::json_report(&summaries, "sweep", 1, 32);
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    let names: Vec<&str> = parsed["providers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["variant"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["short", "json"]);
}

#[test]
fn variants_registry_and_parse() {
    let all = variants::builtins();
    assert_eq!(all.len(), 5);
    for v in &all {
        assert!(!v.prompt.is_empty());
        assert!(!v.description.is_empty());
    }

    assert_eq!(variants::parse("short").unwrap().len(), 1);
    assert_eq!(variants::parse("short, long,,json").unwrap().len(), 3);
    assert_eq!(variants::parse("short,short").unwrap().len(), 1);

    let err = variants::parse("nope").unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("unknown variant 'nope'"));
    for v in &all {
        assert!(msg.contains(v.name));
    }
    assert!(variants::parse(",,  ").is_err());
}

#[tokio::test]
async fn markdown_without_variants_is_unchanged_shape() {
    let config = BenchConfig {
        prompt: "plain".into(),
        runs: 1,
        warmup: 0,
        ..BenchConfig::default()
    };
    let (handle, progress) = spawn_lane(
        reqwest::Client::new(),
        ProviderSpec::mock(),
        "mock-1".into(),
        config,
    );
    drop(progress);
    let summary = handle.await.unwrap();
    assert!(summary.variant.is_none());
    let md = report::markdown(std::slice::from_ref(&summary), "plain", 1, 32);
    assert!(!md.contains("variant: `"));
    assert!(md.contains("| 1 | mock |"));
}
