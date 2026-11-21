use llmrace::bench::{spawn_lane, BenchConfig, LaneSummary, RunMetrics};
use llmrace::provider::ProviderSpec;
use llmrace::workload::{parse, WorkloadPrompt};

#[test]
fn jsonl_skips_blank_lines_and_keeps_expect() {
    let prompts = parse(
        "\n{\"id\":\"capital\",\"prompt\":\"Name the capital.\",\"expect\":\"Paris\"}\n\n{\"id\":\"sum\",\"prompt\":\"2+2\"}\n",
    )
    .unwrap();
    assert_eq!(prompts.len(), 2);
    assert_eq!(prompts[0].expect.as_deref(), Some("Paris"));
    assert!(prompts[1].expect.is_none());
}

#[test]
fn jsonl_rejects_a_repeated_id_and_an_empty_file() {
    assert!(parse("{\"id\":\"a\",\"prompt\":\"one\"}\n{\"id\":\"a\",\"prompt\":\"two\"}").is_err());
    assert!(parse("\n\n").is_err());
    assert!(parse("{\"id\":\"\",\"prompt\":\"x\"}").is_err());
}

#[test]
fn race_json_names_the_price_date_the_workload_and_the_prompt_hash() {
    let one = LaneSummary {
        concurrency: 1,
        runs: vec![RunMetrics::default()],
        ..LaneSummary::default()
    };
    let json: serde_json::Value = serde_json::from_str(&llmrace::report::json_report(
        std::slice::from_ref(&one),
        "hello",
        1,
        32,
    ))
    .unwrap();
    assert_eq!(json["pricing_as_of"], "2026-11-20");
    assert_eq!(json["workload"], serde_json::json!([]));
    assert_eq!(
        json["prompt_sha256"],
        "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
    );

    let two = LaneSummary {
        concurrency: 1,
        runs: vec![RunMetrics::default(), RunMetrics::default()],
        ..LaneSummary::default()
    };
    let workload = vec![
        WorkloadPrompt {
            id: "a".into(),
            prompt: "one".into(),
            expect: None,
        },
        WorkloadPrompt {
            id: "b".into(),
            prompt: "two".into(),
            expect: None,
        },
    ];
    let json: serde_json::Value = serde_json::from_str(&llmrace::report::json_report_cited(
        std::slice::from_ref(&two),
        "workload file (2 prompts)",
        2,
        32,
        None,
        &llmrace::report::SentPrompts {
            prompt: "one",
            workload: &workload,
            warmup: 1,
        },
    ))
    .unwrap();
    assert_eq!(json["workload"], serde_json::json!(["a", "b"]));
    assert_eq!(
        json["prompt_sha256"],
        "21066d108d5319ecb5a1fc4454f42ef22fc5f1c7df49c31d90294950e0ea8b2c"
    );
}

#[tokio::test]
async fn every_lane_draws_the_same_prompt_each_round() {
    let config = BenchConfig {
        prompt: "unused".into(),
        runs: 2,
        warmup: 1,
        workload: vec![
            WorkloadPrompt {
                id: "a".into(),
                prompt: "one".into(),
                expect: None,
            },
            WorkloadPrompt {
                id: "b".into(),
                prompt: "two".into(),
                expect: None,
            },
        ],
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
    let ids: Vec<&str> = summary.runs.iter().map(|r| r.prompt_id.as_str()).collect();
    assert_eq!(ids, ["a", "b"]);
    let md = llmrace::report::markdown(std::slice::from_ref(&summary), "workload", 2, 256);
    assert!(md.contains("## Per prompt"), "{md}");
    assert!(md.contains("`b`"), "{md}");
    let json: serde_json::Value = serde_json::from_str(&llmrace::report::json_report(
        std::slice::from_ref(&summary),
        "w",
        2,
        256,
    ))
    .unwrap();
    assert_eq!(json["providers"][0]["prompts"][1]["id"], "b");
}

#[tokio::test]
async fn an_expected_answer_is_checked_without_failing_the_http_call() {
    let expect = "token ".repeat(24);
    let expect = expect.trim();
    let config = BenchConfig {
        prompt: "unused".into(),
        runs: 1,
        warmup: 0,
        workload: vec![WorkloadPrompt {
            id: "sum".into(),
            prompt: "2+2".into(),
            expect: Some(expect.into()),
        }],
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
    assert!(summary.successful());
    assert_eq!(summary.runs[0].task_ok, Some(true));
    assert_eq!(summary.task_pass(), Some((1, 1)));
    let md = llmrace::report::markdown(std::slice::from_ref(&summary), "w", 1, 256);
    assert!(md.contains("| 1/1 |"), "{md}");
}
