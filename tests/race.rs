use llmrace::bench::{LaneSummary, RunMetrics};
use llmrace::cost::{PriceBook, TokenRates};
use llmrace::provider::ProviderSpec;
use llmrace::race::{resolve_model, GatePolicy, Race};

fn rates(input: f64, output: f64) -> TokenRates {
    TokenRates {
        input,
        output,
        cache_read: None,
        cache_write: None,
        cache_write_1h: None,
    }
}

#[tokio::test]
async fn race_builder_collects_renders_and_gates() {
    let outcome = Race::new([ProviderSpec::mock(), ProviderSpec::mock()])
        .runs(2)
        .warmup(0)
        .run()
        .await
        .unwrap();
    assert_eq!(outcome.summaries.len(), 2);
    assert!(outcome.summaries.iter().all(|s| s.successful()));
    assert_eq!(outcome.actual_runs(), 2);
    assert!(outcome.markdown().contains("llmrace report"));
    let json: serde_json::Value = serde_json::from_str(&outcome.json()).unwrap();
    assert_eq!(
        json["request_sha256"].as_str().unwrap(),
        outcome.request_sha256
    );
    let baseline = llmrace::regression::BaselineReport::from_json_str(&outcome.json()).unwrap();
    let policy = GatePolicy::ci_default();
    let gate = outcome.gate(&baseline, policy);
    assert!(!gate.request_mismatch);
    assert!(
        !gate.gate_failed(policy.require_significance),
        "{}",
        gate.terminal_table(policy.max_regression_pct)
    );
}

#[tokio::test]
async fn winner_podium_scores_two_mock_lanes() {
    let outcome = Race::new([ProviderSpec::mock(), ProviderSpec::mock()])
        .runs(1)
        .warmup(0)
        .winner(llmrace::score::Weights::default())
        .run()
        .await
        .unwrap();
    let podium = outcome.podium();
    assert!(podium.contains("mock-1"), "{podium}");
    assert!(outcome.markdown().contains("## Podium"));
}

#[test]
fn price_book_is_scoped_and_still_sees_the_builtin_table() {
    let mut book = PriceBook::new();
    book.insert("custom/unlisted", rates(3.0, 9.0));
    assert_eq!(
        book.rates("custom", "unlisted")
            .map(|r| (r.input, r.output)),
        Some((3.0, 9.0))
    );
    assert!(
        book.rates("openai", "gpt-4o-mini").is_some(),
        "built-in table stays visible"
    );
    llmrace::cost::set_overrides(vec![("global/only".into(), 1.0, 1.0)]);
    assert!(llmrace::cost::token_rates("global", "only").is_some());
    assert!(
        book.rates("global", "only").is_none(),
        "a book must not read the process-global override map"
    );
}

#[test]
fn a_lane_prices_from_its_own_book_not_the_global_map() {
    let mut book = PriceBook::new();
    book.insert("mock/mock-1", rates(10.0, 20.0));
    let lane = LaneSummary {
        name: "mock".into(),
        model: "mock-1".into(),
        runs: vec![RunMetrics {
            total_ms: 100.0,
            ttfb_ms: 10.0,
            tokens: 100,
            chars: 400,
            prompt_tokens: 1_000,
            ..RunMetrics::default()
        }],
        price_book: Some(book),
        ..LaneSummary::default()
    };
    let booked = lane.cost_per_run_usd().unwrap();
    assert!(
        (booked - (1_000.0 * 10.0 + 100.0 * 20.0) / 1_000_000.0).abs() < 1e-12,
        "{booked}"
    );
    let unbooked = LaneSummary {
        price_book: None,
        ..lane.clone()
    };
    assert!(
        unbooked.cost_per_run_usd().is_none(),
        "mock has no built-in price and the global map holds no mock row"
    );
}

#[tokio::test]
async fn parallel_races_with_different_books_price_independently() {
    let book = |input: f64| {
        let mut book = PriceBook::new();
        book.insert("mock/mock-1", rates(input, 0.0));
        book
    };
    let cheap = Race::new([ProviderSpec::mock()])
        .prompt("hello world")
        .runs(1)
        .warmup(0)
        .price_book(book(1.0))
        .run();
    let pricey = Race::new([ProviderSpec::mock()])
        .prompt("hello world")
        .runs(1)
        .warmup(0)
        .price_book(book(4.0))
        .run();
    let (cheap, pricey) = tokio::join!(cheap, pricey);
    let cheap = cheap.unwrap().summaries[0].cost_per_run_usd().unwrap();
    let pricey = pricey.unwrap().summaries[0].cost_per_run_usd().unwrap();
    assert!(cheap > 0.0, "{cheap}");
    assert!(
        (pricey - 4.0 * cheap).abs() < 1e-15,
        "pricey {pricey} must be 4x cheap {cheap}"
    );
}

#[tokio::test]
async fn spawn_streams_live_progress_before_finish() {
    let mut handle = Race::new([ProviderSpec::mock(), ProviderSpec::mock()])
        .runs(2)
        .warmup(0)
        .spawn()
        .unwrap();
    let progress = handle.progress().unwrap();
    let seen = std::thread::spawn(move || {
        let mut updates = 0;
        let mut finished = 0;
        while let Ok(update) = progress.recv() {
            updates += 1;
            finished += usize::from(update.finished);
        }
        (updates, finished)
    });
    let outcome = handle.finish().await.unwrap();
    let (updates, finished) = seen.join().unwrap();
    assert!(updates > 0, "live updates flow before finish");
    assert_eq!(finished, 2, "both lanes report a final update");
    assert!(outcome.summaries.iter().all(|s| s.successful()));
}

#[tokio::test]
async fn stop_ends_the_race_after_the_in_flight_round() {
    let mut handle = Race::new([ProviderSpec::mock()])
        .runs(100)
        .warmup(0)
        .spawn()
        .unwrap();
    drop(handle.progress());
    handle.stop();
    let outcome = handle.finish().await.unwrap();
    assert!(
        outcome.summaries[0].runs.len() < 100,
        "the stop handle ends the race early: {} rounds",
        outcome.summaries[0].runs.len()
    );
}

#[test]
fn resolve_model_matches_the_cli_rule() {
    let overrides = vec!["ollama=llama3.2".to_string(), "gpt-4o-mini".to_string()];
    assert_eq!(resolve_model(&overrides, "openai", "gpt-4o"), "gpt-4o-mini");
    assert_eq!(resolve_model(&overrides, "ollama", "llama3.1"), "llama3.2");
    assert_eq!(resolve_model(&[], "groq", "llama-3.3-70b"), "llama-3.3-70b");
}

#[tokio::test]
async fn warmup_mode_probe_changes_experiment_sha256() {
    let same = Race::new([ProviderSpec::mock()])
        .runs(1)
        .warmup(1)
        .run()
        .await
        .unwrap();
    let probe = Race::new([ProviderSpec::mock()])
        .runs(1)
        .warmup(1)
        .warmup_mode(llmrace::bench::WarmupMode::ConnectionProbe)
        .run()
        .await
        .unwrap();
    let same_json: serde_json::Value = serde_json::from_str(&same.json()).unwrap();
    let probe_json: serde_json::Value = serde_json::from_str(&probe.json()).unwrap();
    assert_eq!(same_json["experiment"]["warmup_mode"], "same-prompt");
    assert_eq!(probe_json["experiment"]["warmup_mode"], "probe");
    assert_ne!(
        same_json["experiment_sha256"].as_str().unwrap(),
        probe_json["experiment_sha256"].as_str().unwrap(),
        "changing warmup_mode must change experiment_sha256"
    );
}

#[tokio::test]
async fn taskless_podium_omits_task_weight_from_header() {
    let outcome = Race::new([ProviderSpec::mock(), ProviderSpec::mock()])
        .runs(1)
        .warmup(0)
        .winner(llmrace::score::Weights::default())
        .run()
        .await
        .unwrap();
    let podium = outcome.podium();
    assert!(podium.contains("mock-1"), "{podium}");
    assert!(
        !podium.contains("/ task 40"),
        "taskless podium must not print unused task weight: {podium}"
    );
}

#[tokio::test]
async fn gate_policy_fail_on_inconclusive_works_through_race_outcome() {
    let outcome = Race::new([ProviderSpec::mock()])
        .runs(1)
        .warmup(0)
        .run()
        .await
        .unwrap();
    let baseline = llmrace::regression::BaselineReport::from_json_str(
        r#"{"providers":[{"provider":"mock","model":"mock-1","mean_total_ms":10000,"p95_total_ms":1,"mean_ttfb_ms":10000,"mean_tokens_per_sec":0.001}]}"#,
    )
    .unwrap();
    let policy = GatePolicy {
        fail_on_inconclusive: true,
        ..GatePolicy::default()
    };
    let gate = outcome.gate(&baseline, policy);
    assert!(policy.fail_on_inconclusive);
    assert!(gate.gate_failed_with_policy(policy.require_significance, policy.fail_on_inconclusive));
    let lenient = GatePolicy {
        require_significance: true,
        fail_on_inconclusive: false,
        ..GatePolicy::default()
    };
    assert!(gate.inconclusive(lenient.require_significance));
    assert!(
        !gate.gate_failed_with_policy(lenient.require_significance, lenient.fail_on_inconclusive)
    );
    assert!(gate.gate_failed_with_policy(lenient.require_significance, true));
}
