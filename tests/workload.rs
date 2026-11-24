use llmrace::bench::{spawn_lane, BenchConfig, LaneSummary, RunMetrics};
use llmrace::provider::ProviderSpec;
use llmrace::workload::{parse, ExpectMatch, WorkloadPrompt};

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
        WorkloadPrompt::text("a", "one"),
        WorkloadPrompt::text("b", "two"),
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
        &llmrace::report::request_sha256(
            "one",
            &workload,
            &llmrace::provider::Sampling::default(),
            32,
            None,
        ),
    ))
    .unwrap();
    assert_eq!(json["workload"], serde_json::json!(["a", "b"]));
    assert_eq!(
        json["prompt_sha256"],
        "21066d108d5319ecb5a1fc4454f42ef22fc5f1c7df49c31d90294950e0ea8b2c"
    );
    let plain = llmrace::report::request_sha256(
        "hello",
        &[],
        &llmrace::provider::Sampling::default(),
        32,
        None,
    );
    let sampling = llmrace::provider::Sampling {
        system: Some("be brief".into()),
        ..llmrace::provider::Sampling::default()
    };
    let with_system = llmrace::report::request_sha256("hello", &[], &sampling, 32, None);
    assert_ne!(plain, with_system);
    assert_ne!(
        plain,
        llmrace::report::request_sha256(
            "hello",
            &[],
            &llmrace::provider::Sampling::default(),
            64,
            None
        )
    );
    assert_ne!(
        llmrace::report::request_sha256(
            "one",
            &workload,
            &llmrace::provider::Sampling::default(),
            32,
            None
        ),
        {
            let mut changed = workload.clone();
            changed[0].expect = Some("Paris".into());
            llmrace::report::request_sha256(
                "one",
                &changed,
                &llmrace::provider::Sampling::default(),
                32,
                None,
            )
        }
    );
}

#[tokio::test]
async fn every_lane_draws_the_same_prompt_each_round() {
    let config = BenchConfig {
        prompt: "unused".into(),
        runs: 2,
        warmup: 1,
        workload: vec![
            WorkloadPrompt::text("a", "one"),
            WorkloadPrompt::text("b", "two"),
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
            expect: Some(expect.into()),
            ..WorkloadPrompt::text("sum", "2+2")
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

#[test]
fn match_rejects_an_unknown_mode_and_a_bad_regex_before_any_request() {
    let unknown = parse("{\"id\":\"a\",\"prompt\":\"q\",\"expect\":\"x\",\"match\":\"fuzzy\"}");
    assert!(unknown.is_err(), "{unknown:?}");
    let bad = parse("{\"id\":\"a\",\"prompt\":\"q\",\"expect\":\"(\",\"match\":\"regex\"}");
    assert!(bad.is_err(), "{bad:?}");
    let exact = parse("{\"id\":\"a\",\"prompt\":\"q\",\"expect\":\"Paris\"}").unwrap();
    assert_eq!(exact[0].expect_match, ExpectMatch::Exact);
    let contains =
        parse("{\"id\":\"a\",\"prompt\":\"q\",\"expect\":\"Paris\",\"match\":\"contains\"}")
            .unwrap();
    assert_eq!(contains[0].expect_match, ExpectMatch::Contains);
}

#[test]
fn a_conversation_a_schema_and_a_tool_parse_and_change_the_hash() {
    let conversation = parse(
        r#"{"id":"capital","system":"Be brief.","messages":[{"role":"user","content":"Capital?"},{"role":"assistant","content":"Of where?"},{"role":"user","content":"France."}],"expect":"{\"type\":\"object\",\"required\":[\"city\"],\"properties\":{\"city\":{\"type\":\"string\"}},\"additionalProperties\":false}","match":"json_schema"}"#,
    )
    .unwrap();
    assert_eq!(conversation[0].prompt, "France.");
    assert_eq!(conversation[0].messages.len(), 3);
    assert!(llmrace::tools::answer_matches_mode(
        r#"{"city":"Paris"}"#,
        conversation[0].expect.as_deref().unwrap(),
        ExpectMatch::JsonSchema,
    ));
    assert!(!llmrace::tools::answer_matches_mode(
        r#"{"city":"Paris","extra":1}"#,
        conversation[0].expect.as_deref().unwrap(),
        ExpectMatch::JsonSchema,
    ));
    let plain = parse(r#"{"id":"capital","prompt":"France."}"#).unwrap();
    let sampling = llmrace::provider::Sampling::default();
    assert_ne!(
        llmrace::report::request_sha256("France.", &plain, &sampling, 32, None),
        llmrace::report::request_sha256("France.", &conversation, &sampling, 32, None)
    );
    assert!(parse(r#"{"id":"a","messages":[{"role":"assistant","content":"hi"}]}"#).is_err());
    assert!(parse(
        r#"{"id":"a","prompt":"q","expect":"{\"type\":\"object\",\"pattern\":\"a\"}","match":"json_schema"}"#
    )
    .is_err());
    let tool = parse(
        r#"{"id":"weather","prompt":"Weather in Paris?","tool":{"name":"get_weather","description":"lookup","parameters":{"type":"object","required":["city"],"properties":{"city":{"type":"string"}}}}}"#,
    )
    .unwrap();
    assert_eq!(
        llmrace::tools::task_ok_with_tool(
            true,
            "weather",
            r#"{"city":"Paris"}"#,
            Some(&tool[0].tool.as_ref().unwrap().parameters),
            None,
            ExpectMatch::Exact,
            "",
        ),
        Some(true)
    );
    assert_eq!(
        llmrace::tools::task_ok_with_tool(
            true,
            "weather",
            r#"{"nope":1}"#,
            Some(&tool[0].tool.as_ref().unwrap().parameters),
            None,
            ExpectMatch::Exact,
            "",
        ),
        Some(false)
    );
}

#[test]
fn an_exact_workload_keeps_its_request_hash() {
    let exact = parse("{\"id\":\"a\",\"prompt\":\"one\",\"expect\":\"Paris\"}").unwrap();
    let marked =
        parse("{\"id\":\"a\",\"prompt\":\"one\",\"expect\":\"Paris\",\"match\":\"exact\"}")
            .unwrap();
    let contains =
        parse("{\"id\":\"a\",\"prompt\":\"one\",\"expect\":\"Paris\",\"match\":\"contains\"}")
            .unwrap();
    let sampling = llmrace::provider::Sampling::default();
    let exact_hash = llmrace::report::request_sha256("one", &exact, &sampling, 32, None);
    assert_eq!(
        exact_hash,
        llmrace::report::request_sha256("one", &marked, &sampling, 32, None)
    );
    assert_ne!(
        exact_hash,
        llmrace::report::request_sha256("one", &contains, &sampling, 32, None)
    );
}

#[tokio::test]
async fn contains_and_regex_score_the_mock_answer() {
    async fn once(expect: &str, mode: ExpectMatch) -> Option<bool> {
        let config = BenchConfig {
            prompt: "unused".into(),
            runs: 1,
            warmup: 0,
            workload: vec![WorkloadPrompt {
                expect: Some(expect.into()),
                expect_match: mode,
                ..WorkloadPrompt::text("sum", "2+2")
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
        summary.runs[0].task_ok
    }
    assert_eq!(once("token", ExpectMatch::Contains).await, Some(true));
    assert_eq!(once("TOKEN", ExpectMatch::Contains).await, Some(false));
    assert_eq!(
        once(r"^token( token)*$", ExpectMatch::Regex).await,
        Some(true)
    );
    assert_eq!(once("nope", ExpectMatch::Regex).await, Some(false));
}
