use llmrace::bench::{LaneSummary, RunMetrics};
use llmrace::regression::{compare_baselines, evaluate, BaselineReport};

fn lane(name: &str, model: &str, latency: f64, tokens: u64) -> LaneSummary {
    LaneSummary {
        name: name.into(),
        model: model.into(),
        concurrency: 1,
        runs: [-1.0, 0.0, 1.0]
            .iter()
            .map(|offset| RunMetrics {
                total_ms: latency + offset,
                ttfb_ms: 10.0,
                tokens,
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    }
}

#[test]
fn self_comparison_and_lane_permutation_preserve_model_identity() {
    let lanes = [
        lane("same", "fast", 100.0, 10),
        lane("same", "slow", 1000.0, 100),
    ];
    let report = llmrace::report::json_report(&lanes, "p", 3, 256);
    let baseline = BaselineReport::from_json_str(&report).unwrap();
    assert!(!compare_baselines(&baseline, &baseline, 15.0).gate_failed(false));
    let mut reversed = baseline.clone();
    reversed.providers.reverse();
    let compared = compare_baselines(&reversed, &baseline, 15.0);
    assert!(!compared.gate_failed(false));
    assert!(compared
        .comparisons
        .iter()
        .all(|c| c.baseline_model == c.current_model));
}

#[test]
fn failed_and_missing_lanes_fail_even_with_significance_required() {
    let healthy = lane("a", "m", 100.0, 10);
    let baseline = BaselineReport::from_json_str(&llmrace::report::json_report(
        std::slice::from_ref(&healthy),
        "p",
        3,
        256,
    ))
    .unwrap();
    let failed = LaneSummary {
        name: "a".into(),
        model: "m".into(),
        error: Some("outage".into()),
        ..Default::default()
    };
    for current in [vec![failed], vec![]] {
        let result = evaluate(&current, &baseline, 15.0);
        assert!(result.gate_failed(true));
        assert_eq!(
            result.verdict_json(15.0, true)["failures"][0]["metric"],
            "availability"
        );
    }
    assert!(BaselineReport::from_json_str(r#"{"providers":[]}"#).is_err());
}

#[test]
fn changed_concurrency_is_not_a_performance_comparison() {
    let a = lane("a", "m", 100.0, 10);
    let baseline = BaselineReport::from_json_str(&llmrace::report::json_report(
        std::slice::from_ref(&a),
        "p",
        3,
        256,
    ))
    .unwrap();
    let mut b = a;
    b.concurrency = 8;
    let comparison = evaluate(&[b], &baseline, 15.0);
    assert!(comparison.gate_failed(true));
    assert_eq!(comparison.comparisons[0].metrics[0].label, "configuration");
}

#[test]
fn missing_or_fully_failed_baseline_prompts_cannot_silently_pass() {
    let baseline = BaselineReport::from_json_str(r#"{"providers":[{"provider":"a","model":"m","prompts":[{"id":"required","rounds":1,"mean_total_ms":100}]}]}"#).unwrap();
    for prompts in [
        serde_json::json!([]),
        serde_json::json!([{"id":"required","rounds":0}]),
    ] {
        let current = BaselineReport::from_json_str(&serde_json::json!({"providers":[{
            "provider":"a", "model":"m", "successful":true, "failure_rate":0.001, "prompts":prompts
        }]}).to_string()).unwrap();
        let verdict = compare_baselines(&current, &baseline, 15.0).verdict_json(15.0, true);
        assert_eq!(verdict["ok"], false);
        assert!(verdict["failures"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["prompt_id"] == "required" && f["metric"] == "availability"));
    }
}

#[test]
fn latency_verdict_does_not_confuse_throughput_with_low_latency() {
    let lanes = [
        lane("high-throughput", "a", 1000.0, 1000),
        lane("low-latency", "b", 100.0, 1),
    ];
    let verdict = llmrace::report::verdict_line(&lanes).unwrap();
    assert!(
        verdict.contains("high-throughput leads on goodput"),
        "{verdict}"
    );
    assert!(verdict.contains("10.0× slower"), "{verdict}");
    assert!(!verdict.contains("within 10%"), "{verdict}");
    assert!(verdict.contains("mean-latency difference"), "{verdict}");
}

#[test]
fn student_t_interval_keeps_its_finite_sample_width_at_df30() {
    let sample = llmrace::stats::Sample::from_values((0..31).map(f64::from).collect());
    let (lo, hi) = sample.t_ci95().unwrap();
    let critical = (hi - lo) / 2.0 / (sample.stddev() / 31_f64.sqrt());
    assert!((critical - 2.042272456).abs() < 1e-8, "{critical}");
}

#[test]
fn untestable_tail_regression_is_explicitly_inconclusive() {
    let report = |p95| {
        BaselineReport::from_json_str(
            &serde_json::json!({"providers": [{
                "provider": "a", "model": "m", "mean_total_ms": 100.0, "p95_total_ms": p95,
                "mean_ttfb_ms": 10.0, "mean_tokens_per_sec": 100.0,
            }]})
            .to_string(),
        )
        .unwrap()
    };
    let comparison = compare_baselines(&report(1000.0), &report(100.0), 15.0);
    assert_eq!(comparison.status(true), "inconclusive");
    assert!(comparison.gate_failed_with_policy(true, true));
    assert!(!comparison.gate_failed_with_policy(true, false));
    let strict = comparison.verdict_json_with_policy(15.0, true, true);
    assert_eq!(strict["ok"], false);
    assert_eq!(strict["inconclusive"][0]["metric"], "p95 latency");
    assert!(!comparison
        .markdown_section(15.0, true)
        .contains("treating as noise"));
}

#[tokio::test]
async fn concurrent_workload_citation_and_experiment_identity_are_complete() {
    use llmrace::provider::ProviderSpec;
    use llmrace::race::{GatePolicy, Race};
    let workload = (0..3)
        .map(|i| llmrace::workload::WorkloadPrompt::text(i.to_string(), i.to_string()))
        .collect();
    let race = Race::new([ProviderSpec::mock()])
        .workload(workload)
        .runs(3)
        .warmup(0)
        .concurrency(3);
    let outcome = race.clone().run().await.unwrap();
    let json = outcome.json();
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value["workload"], serde_json::json!(["0", "1", "2"]));
    assert_eq!(value["providers"][0]["measured_requests"], 9);
    let baseline = BaselineReport::from_json_str(&json).unwrap();
    let changed = race.retries(1).runs(1).run().await.unwrap();
    assert!(
        changed
            .gate(&baseline, GatePolicy::ci_default())
            .request_mismatch
    );
    assert!(Race::new([]).spawn().is_err());
}

#[test]
fn recommendation_uses_compatible_history_and_thirty_days_of_cost() {
    let mut latest = llmrace::history::record_from_summary(&lane("a", "new", 100.0, 10), 10);
    latest.cost_per_run_usd = Some(0.001);
    latest.request_sha256 = Some("current".into());
    let mut old = latest.clone();
    old.timestamp_unix = 1;
    old.model = "old".into();
    old.mean_total_ms = 10000.0;
    let profiles = llmrace::recommend::build_profiles(&[latest, old]);
    assert_eq!(profiles[0].races, 1);
    assert_eq!(profiles[0].median_latency_ms, 100.0);
    assert_eq!(profiles[0].monthly_cost_usd, Some(30.0));
}
