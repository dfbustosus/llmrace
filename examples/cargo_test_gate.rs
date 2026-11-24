//! Race providers and gate the result from your own crate.
//!
//! Run it: `cargo run --example cargo_test_gate`. In a real project the body
//! of `gate_is_green` is a `#[tokio::test]`: record the baseline once with
//! `outcome.json()`, commit it, and assert the gate stays green.

use llmrace::provider::ProviderSpec;
use llmrace::race::{GatePolicy, Race};
use llmrace::workload::{ExpectMatch, WorkloadPrompt};

#[tokio::main]
async fn main() {
    gate_is_green().await;
}

async fn gate_is_green() {
    let workload = vec![
        WorkloadPrompt {
            expect: Some("token".into()),
            expect_match: ExpectMatch::Contains,
            ..WorkloadPrompt::text("answer", "Say anything.")
        },
        WorkloadPrompt {
            expect: Some(
                r#"{"type":"object","required":["city"],"properties":{"city":{"type":"string"}}}"#
                    .into(),
            ),
            expect_match: ExpectMatch::JsonSchema,
            ..WorkloadPrompt::text("city", "Name a city as JSON.")
        },
    ];
    let outcome = Race::new([ProviderSpec::mock(), ProviderSpec::mock()])
        .workload(workload)
        .runs(4)
        .warmup(0)
        .reproducible()
        .run()
        .await
        .expect("race runs offline on the mock provider");

    // Day one: commit this file as the baseline.
    let baseline = llmrace::regression::BaselineReport::from_json_str(&outcome.json())
        .expect("the race report parses as a baseline");

    // In CI: gate the same workload against the committed baseline.
    let policy = GatePolicy::ci_default();
    let gate = outcome.gate(&baseline, policy);
    assert!(
        !gate.gate_failed(policy.require_significance),
        "{}",
        gate.markdown_section(policy.max_regression_pct, policy.require_significance)
    );
    println!(
        "gate ok — {} lanes, request {}",
        outcome.summaries.len(),
        &outcome.request_sha256[..12]
    );
}
