//! Machine-readable JSON report rendering and optional metadata injection.

use super::{
    citation, counted_rounds, experiment_manifest_with, hash_experiment_manifest, request_sha256,
    top_two_significance, verdict_line, SentPrompts,
};
use crate::bench::LaneSummary;

/// Optional metadata sections embedded into a JSON report in a single pass.
#[derive(Debug, Default, Clone)]
pub struct JsonReportOptions<'a> {
    /// Optional probe region tag (`--region`).
    pub region: Option<&'a str>,
    /// Optional prompt/workload citation context.
    pub sent: Option<SentPrompts<'a>>,
    /// Optional precomputed `request_sha256` override.
    pub request_sha256: Option<&'a str>,
    /// Optional sampling controls stamped under `"sampling"` when non-empty.
    pub sampling: Option<&'a crate::provider::Sampling>,
    /// Optional experiment config and resolved `(variant_name, prompt)` pairs.
    pub experiment: Option<(&'a crate::bench::BenchConfig, &'a [(&'a str, &'a str)])>,
    /// Optional regression gate verdict stamped under `"regression"`.
    pub regression: Option<&'a serde_json::Value>,
}

/// JSON object of the sampling controls that were set, suitable for
/// inserting into a race report. Unset controls are omitted.
#[must_use]
pub fn sampling_json(sampling: &crate::provider::Sampling) -> Option<serde_json::Value> {
    if sampling.is_unset() {
        return None;
    }
    let mut obj = serde_json::Map::new();
    if let Some(t) = sampling.temperature {
        obj.insert("temperature".into(), serde_json::json!(t));
    }
    if let Some(p) = sampling.top_p {
        obj.insert("top_p".into(), serde_json::json!(p));
    }
    if let Some(seed) = sampling.seed {
        obj.insert("seed".into(), serde_json::json!(seed));
    }
    if let Some(sys) = sampling.system.as_deref().filter(|s| !s.is_empty()) {
        obj.insert("system".into(), serde_json::Value::String(sys.to_string()));
    }
    if let Some(json_output) = &sampling.json_output {
        obj.insert(
            "json_output".into(),
            match json_output {
                crate::provider::JsonOutput::Object => serde_json::json!("object"),
                crate::provider::JsonOutput::Schema(_) => serde_json::json!("schema"),
            },
        );
    }
    Some(serde_json::Value::Object(obj))
}

fn apply_sampling(
    map: &mut serde_json::Map<String, serde_json::Value>,
    sampling: &crate::provider::Sampling,
) {
    if let Some(value) = sampling_json(sampling) {
        map.insert("sampling".to_string(), value);
    }
}

fn apply_experiment<S: serde::Serialize>(
    map: &mut serde_json::Map<String, serde_json::Value>,
    config: &crate::bench::BenchConfig,
    variants: &[(S, S)],
) {
    let manifest = experiment_manifest_with(config, variants);
    let hash = hash_experiment_manifest(&manifest);
    map.insert("experiment_sha256".to_string(), serde_json::json!(hash));
    map.insert("experiment".to_string(), manifest);
    map.insert("measurement_version".to_string(), serde_json::json!(2));
}

fn apply_regression(
    map: &mut serde_json::Map<String, serde_json::Value>,
    verdict: &serde_json::Value,
) {
    map.insert("regression".to_string(), verdict.clone());
}

/// Inserts a `sampling` object into an already-rendered JSON report.
///
/// Returns the input unchanged when sampling is unset or the document is
/// not an object, so older call sites keep their bytes.
#[must_use]
pub fn insert_sampling_json(json: &str, sampling: &crate::provider::Sampling) -> String {
    if sampling.is_unset() {
        return json.to_string();
    }
    let Ok(mut doc) = serde_json::from_str::<serde_json::Value>(json) else {
        return json.to_string();
    };
    if let Some(map) = doc.as_object_mut() {
        apply_sampling(map, sampling);
    }
    serde_json::to_string_pretty(&doc).unwrap_or_else(|_| json.to_string())
}

/// Inserts the baseline-gate verdict into a JSON report.
///
/// Present only when the race was compared to a `--baseline`. An older
/// report omits the key and still validates.
#[must_use]
pub fn insert_regression_json(json: &str, verdict: &serde_json::Value) -> String {
    let Ok(mut doc) = serde_json::from_str::<serde_json::Value>(json) else {
        return json.to_string();
    };
    if let Some(map) = doc.as_object_mut() {
        apply_regression(map, verdict);
    }
    serde_json::to_string_pretty(&doc).unwrap_or_else(|_| json.to_string())
}

/// Adds the execution identity used for baseline compatibility checks.
pub fn insert_experiment_json(
    json: &str,
    config: &crate::bench::BenchConfig,
    variants: &[(String, String)],
) -> String {
    let Ok(mut doc) = serde_json::from_str::<serde_json::Value>(json) else {
        return json.to_string();
    };
    if let Some(map) = doc.as_object_mut() {
        apply_experiment(map, config, variants);
    }
    serde_json::to_string_pretty(&doc).unwrap_or_else(|_| json.to_string())
}

/// Renders the full machine-readable JSON report.
pub fn json_report(
    summaries: &[LaneSummary],
    prompt: &str,
    runs: usize,
    max_tokens: u64,
) -> String {
    render_json_report(
        summaries,
        prompt,
        runs,
        max_tokens,
        &JsonReportOptions::default(),
    )
}

/// Renders the JSON report with an optional region tag (`--region`), used
/// by distributed probes so merged reports stay attributable.
pub fn json_report_with_region(
    summaries: &[LaneSummary],
    prompt: &str,
    runs: usize,
    max_tokens: u64,
    region: Option<&str>,
) -> String {
    render_json_report(
        summaries,
        prompt,
        runs,
        max_tokens,
        &JsonReportOptions {
            region,
            ..JsonReportOptions::default()
        },
    )
}

/// Renders the JSON report for a race that may have drawn a workload.
///
/// `prompt` stays the header label. [`SentPrompts`] is what each measured
/// round actually sent.
#[must_use]
pub fn json_report_cited(
    summaries: &[LaneSummary],
    prompt: &str,
    runs: usize,
    max_tokens: u64,
    region: Option<&str>,
    sent: &SentPrompts<'_>,
    request_sha256: &str,
) -> String {
    render_json_report(
        summaries,
        prompt,
        runs,
        max_tokens,
        &JsonReportOptions {
            region,
            sent: Some(sent.clone()),
            request_sha256: Some(request_sha256),
            ..JsonReportOptions::default()
        },
    )
}

/// Renders a JSON report in a single serialization pass with any requested
/// optional sections (`sampling`, `experiment`, `regression`).
#[must_use]
pub fn render_json_report(
    summaries: &[LaneSummary],
    prompt: &str,
    runs: usize,
    max_tokens: u64,
    opts: &JsonReportOptions<'_>,
) -> String {
    let default_sent = SentPrompts {
        prompt,
        workload: &[],
        warmup: 0,
    };
    let sent = opts.sent.as_ref().unwrap_or(&default_sent);
    let default_sampling = crate::provider::Sampling::default();
    let request_sha: std::borrow::Cow<'_, str> = match opts.request_sha256 {
        Some(hash) => std::borrow::Cow::Borrowed(hash),
        None => {
            let sampling = opts
                .sampling
                .or_else(|| opts.experiment.map(|(cfg, _)| &cfg.sampling))
                .unwrap_or(&default_sampling);
            let tools = opts
                .experiment
                .and_then(|(cfg, _)| cfg.tools.then_some(cfg.tool_task.as_str()));
            std::borrow::Cow::Owned(request_sha256(
                sent.prompt,
                sent.workload,
                sampling,
                max_tokens,
                tools,
            ))
        }
    };
    let (workload_ids, prompt_sha256) = citation(sent, counted_rounds(summaries, runs));
    let providers: Vec<serde_json::Value> = summaries
        .iter()
        .map(|s| {
            let tpot = s.tpot_ms();
            let mut value = serde_json::json!({
                "provider": s.name,
                "model": s.model,
                "variant": s.variant,
                "concurrency": s.concurrency,
                "successful": s.successful(),
                "error": s.error,
                "measured_rounds": s.measured_rounds(),
                "failed_rounds": s.failed_rounds(),
                "failure_rate": s.failure_rate(),
                "retry_attempts": s.retry_attempts,
                "goodput_tokens_per_sec": s.goodput_tokens_per_sec(),
                "failures": s.failures,
                "runs": s.runs.iter().map(run_json).collect::<Vec<_>>(),
                "mean_total_ms": s.mean_total_ms(),
                "median_total_ms": s.median_total_ms(),
                "p95_total_ms": s.p95_total_ms(),
                "ci95_total_ms": s.ci95_total_ms(),
                "mean_ttfb_ms": s.mean_ttfb_ms(),
                "ttfb_p50_ms": s.ttfb_p50_ms(),
                "ttfb_p95_ms": s.ttfb_p95_ms(),
                "mean_tokens_per_sec": s.mean_tokens_per_sec(),
                "outlier_runs": s.outlier_runs(),
                "cost_per_run_usd": s.cost_per_run_usd(),
                "prompt_tokens": s.mean_prompt_tokens(),
                "prompt_tokens_measured": s.prompt_tokens_measured(),
                "cache_read_tokens": s.mean_cache_read_tokens(),
                "cache_write_tokens": s.mean_cache_write_tokens(),
                "cache_write_1h_tokens": s.mean_cache_write_1h_tokens(),
                "cache_discount_applied": s.cache_discount_applied(),
                "cache_hit_rounds": s.cache_hit_rounds(),
                "server_ms": s.median_server_ms(),
                "network_overhead_ms": s.network_overhead_ms(),
                "itl_p50_ms": s.itl_p50_ms(),
                "itl_p95_ms": s.itl_p95_ms(),
                "stutters": s.stutters(),
                "visible_tokens": s.visible_tokens(),
                "reasoning_tokens": s.reasoning_tokens(),
                "thinking_ms_estimate": s.thinking_ms_estimate(),
                "request_id": s.last_request_id(),
                "tool_validity": s.tool_validity(),
            });
            value["measured_requests"] = serde_json::json!(s.measured_requests());
            value["failed_requests"] = serde_json::json!(s.failed_requests());
            value["request_failure_rate"] = serde_json::json!(s.request_failure_rate());
            value["end_to_end_tokens_per_sec"] = serde_json::json!(s.end_to_end_tokens_per_sec());
            value["request_ttfb_p50_ms"] = serde_json::json!(s.request_ttfb_p50_ms());
            value["request_ttfb_p95_ms"] = serde_json::json!(s.request_ttfb_p95_ms());
            value["server_total_ms"] = serde_json::json!(s.median_server_total_ms());
            let prompts: Vec<serde_json::Value> = s
                .prompt_rows()
                .into_iter()
                .map(|row| {
                    serde_json::json!({
                        "id": row.id,
                        "rounds": row.rounds,
                        "failed_rounds": row.failed_rounds,
                        "mean_total_ms": (row.rounds > 0).then_some(row.total_ms),
                        "mean_ttfb_ms": (row.rounds > 0).then_some(row.ttfb_ms),
                        "mean_tokens_per_sec": (row.rounds > 0).then_some(row.tokens_per_sec),
                        "task_pass": row.task_pass.map(|(ok, n)| serde_json::json!([ok, n])),
                        "task_pass_rate": row.task_pass.map(|(ok, n)| {
                            if n == 0 { 0.0 } else { ok as f64 / n as f64 }
                        }),
                    })
                })
                .collect();
            if !prompts.is_empty() {
                value["prompts"] = serde_json::Value::Array(prompts);
            }
            value["task_pass"] = match s.task_pass() {
                Some((ok, n)) => serde_json::json!([ok, n]),
                None => serde_json::Value::Null,
            };
            value["task_pass_rate"] = serde_json::json!(s.task_rate());
            value["ttft_ms"] = serde_json::json!(s.mean_ttfb_ms());
            value["tpot_p50_ms"] = serde_json::json!(tpot.map(|(p50, _)| p50));
            value["tpot_p95_ms"] = serde_json::json!(tpot.map(|(_, p95)| p95));
            value
        })
        .collect();
    let mut doc = serde_json::json!({
        "tool": "llmrace",
        "version": env!("CARGO_PKG_VERSION"),
        "region": opts.region,
        "prompt": prompt,
        "pricing_as_of": crate::cost::PRICING_AS_OF,
        "workload": workload_ids,
        "prompt_sha256": prompt_sha256,
        "request_sha256": request_sha.as_ref(),
        "runs": runs,
        "max_tokens": max_tokens,
        "verdict": verdict_line(summaries),
        "significance": top_two_significance(summaries).map(
            |(a, b, delta_ms, p, significant)| {
                serde_json::json!({
                    "a": a,
                    "b": b,
                    "delta_ms": delta_ms,
                    "p_value": p,
                    "significant": significant,
                })
            },
        ),
        "providers": providers,
    });
    if let Some(map) = doc.as_object_mut() {
        if let Some(sampling) = opts.sampling {
            apply_sampling(map, sampling);
        }
        if let Some((config, variants)) = opts.experiment {
            apply_experiment(map, config, variants);
        }
        if let Some(regression) = opts.regression {
            apply_regression(map, regression);
        }
    }
    serde_json::to_string_pretty(&doc).unwrap_or_default()
}

fn run_json(run: &crate::bench::RunMetrics) -> serde_json::Value {
    let mut value = serde_json::to_value(run).unwrap_or_else(|_| serde_json::json!({}));
    value["tokens_per_sec"] = serde_json::json!(run.tokens_per_sec());
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bench::{BenchConfig, RequestAttempt, RequestOutcome, RunMetrics};
    use crate::provider::{JsonOutput, Sampling};

    #[test]
    fn json_and_markdown_surface_request_ttfb_and_server_total_ms() {
        let lane = LaneSummary {
            name: "bedrock".into(),
            model: "claude".into(),
            concurrency: 2,
            runs: vec![RunMetrics {
                requests: vec![
                    RequestOutcome {
                        elapsed_ms: 200.0,
                        attempts: vec![RequestAttempt::Success(RunMetrics {
                            total_ms: 200.0,
                            ttfb_ms: 30.0,
                            tokens: 10,
                            server_total_ms: Some(180.0),
                            ..RunMetrics::default()
                        })],
                    },
                    RequestOutcome {
                        elapsed_ms: 350.0,
                        attempts: vec![RequestAttempt::Success(RunMetrics {
                            total_ms: 350.0,
                            ttfb_ms: 130.0,
                            tokens: 10,
                            server_total_ms: Some(320.0),
                            ..RunMetrics::default()
                        })],
                    },
                ],
                total_ms: 350.0,
                ttfb_ms: 30.0,
                tokens: 20,
                server_total_ms: Some(250.0),
                ..RunMetrics::default()
            }],
            ..LaneSummary::default()
        };

        let raw = json_report(std::slice::from_ref(&lane), "hi", 1, 32);
        let doc: serde_json::Value = serde_json::from_str(&raw).expect("valid json");
        let p = &doc["providers"][0];
        assert_eq!(p["request_ttfb_p50_ms"].as_f64(), Some(80.0));
        assert_eq!(p["request_ttfb_p95_ms"].as_f64(), Some(125.0));
        assert_eq!(p["server_total_ms"].as_f64(), Some(250.0));
        assert_eq!(p["runs"][0]["server_total_ms"].as_f64(), Some(250.0));

        let md = super::super::markdown(&[lane], "hi", 1, 32);
        assert!(
            md.contains("Per-request TTFT under concurrency: bedrock (p50 80 ms, p95 125 ms)."),
            "{md}"
        );
    }

    #[test]
    fn render_json_report_matches_legacy_insert_chain_byte_for_byte() {
        let lane = LaneSummary {
            name: "mock".into(),
            model: "mock-v1".into(),
            concurrency: 1,
            runs: vec![RunMetrics {
                total_ms: 120.0,
                ttfb_ms: 25.0,
                tokens: 30,
                ..RunMetrics::default()
            }],
            ..LaneSummary::default()
        };
        let sampling = Sampling {
            temperature: Some(0.0),
            top_p: Some(1.0),
            seed: Some(42),
            system: Some("Be brief.".into()),
            json_output: Some(JsonOutput::Object),
        };
        let bench = BenchConfig {
            prompt: "hi".into(),
            runs: 1,
            warmup: 0,
            max_tokens: 64,
            sampling: sampling.clone(),
            ..BenchConfig::default()
        };
        let sent = SentPrompts {
            prompt: &bench.prompt,
            workload: &bench.workload,
            warmup: bench.warmup,
        };
        let req = request_sha256(&bench.prompt, &bench.workload, &bench.sampling, 64, None);
        let variants_owned = vec![
            ("short".to_string(), "Reply briefly.".to_string()),
            ("json".to_string(), "Reply in JSON.".to_string()),
        ];
        let variants_borrowed: Vec<(&str, &str)> = variants_owned
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let verdict = serde_json::json!({
            "ok": true,
            "status": "pass",
            "threshold_pct": 15.0,
        });

        let legacy = insert_regression_json(
            &insert_experiment_json(
                &insert_sampling_json(
                    &json_report_cited(
                        std::slice::from_ref(&lane),
                        "hi",
                        1,
                        64,
                        Some("us-east-1"),
                        &sent,
                        &req,
                    ),
                    &sampling,
                ),
                &bench,
                &variants_owned,
            ),
            &verdict,
        );

        let single_pass = render_json_report(
            std::slice::from_ref(&lane),
            "hi",
            1,
            64,
            &JsonReportOptions {
                region: Some("us-east-1"),
                sent: Some(sent),
                request_sha256: Some(&req),
                sampling: Some(&sampling),
                experiment: Some((&bench, &variants_borrowed)),
                regression: Some(&verdict),
            },
        );

        assert_eq!(single_pass, legacy);
    }

    #[test]
    fn render_json_report_omits_unset_optional_sections() {
        let lane = LaneSummary {
            name: "mock".into(),
            model: "mock-v1".into(),
            concurrency: 1,
            runs: vec![RunMetrics {
                total_ms: 100.0,
                ttfb_ms: 20.0,
                tokens: 15,
                ..RunMetrics::default()
            }],
            ..LaneSummary::default()
        };
        let unset = Sampling::default();
        let raw = render_json_report(
            &[lane],
            "hi",
            1,
            32,
            &JsonReportOptions {
                sampling: Some(&unset),
                ..JsonReportOptions::default()
            },
        );
        let doc: serde_json::Value = serde_json::from_str(&raw).expect("valid json");
        let obj = doc.as_object().expect("json object");
        assert!(!obj.contains_key("sampling"), "must not emit null sampling");
        assert!(
            !obj.contains_key("regression"),
            "must not emit null regression"
        );
        assert!(
            !obj.contains_key("experiment"),
            "must not emit null experiment"
        );
        assert!(
            !obj.contains_key("experiment_sha256"),
            "must not emit experiment_sha256 without experiment"
        );
        assert!(
            !obj.contains_key("measurement_version"),
            "must not emit measurement_version without experiment"
        );
    }
}
