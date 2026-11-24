//! SARIF 2.1.0 output for code-scanning integrations.
//!
//! GitHub, Azure DevOps and other code-scanning consumers ingest SARIF
//! natively. Every condition that fails a gate — a regression beyond the
//! threshold, a changed request, a TTFB breach, a lane that never
//! succeeded — becomes one `result`, so an upload step turns the gate into
//! repository alerts with no bot and no parsing.

use crate::bench::LaneSummary;
use crate::regression::RegressionReport;
use serde_json::{json, Value};

/// Renders the SARIF log for one race.
///
/// `regression` is the evaluated baseline gate when `--baseline` was set.
/// `max_ttfb` is the TTFB gate threshold when set. `max_regression` and
/// `require_significance` decide which regression metrics count — the same
/// rule the process exit code uses. A green race emits zero results.
#[must_use]
pub fn sarif(
    summaries: &[LaneSummary],
    regression: Option<(&RegressionReport, f64, bool)>,
    max_ttfb: Option<f64>,
) -> String {
    let mut results: Vec<Value> = Vec::new();

    if let Some((report, threshold, require_significance)) = regression {
        if report.request_mismatch {
            results.push(result(
                "llmrace/request-mismatch",
                "the request changed, so the metrics were not compared (the gate refuses a stale baseline)",
                Vec::new(),
            ));
        }
        for comparison in &report.comparisons {
            for metric in &comparison.metrics {
                if !metric.fails_the_gate(require_significance) {
                    continue;
                }
                let prompt = metric
                    .prompt_id
                    .as_deref()
                    .map(|id| format!(" prompt {id}"))
                    .unwrap_or_default();
                results.push(result(
                    "llmrace/regression",
                    &format!(
                        "{}: {}{} {:.1} → {:.1} ({:+.1}%, threshold {:.0}%)",
                        comparison.name,
                        metric.label,
                        prompt,
                        metric.baseline,
                        metric.current,
                        metric.delta_pct,
                        threshold
                    ),
                    logical(&comparison.name, metric.prompt_id.as_deref()),
                ));
            }
        }
    }

    if let Some(max_ttfb) = max_ttfb {
        for offender in crate::gates::ttfb_offenders(summaries, max_ttfb) {
            results.push(result(
                "llmrace/ttfb-gate",
                &format!(
                    "{}: mean TTFB {:.0} ms exceeds the {:.0} ms gate",
                    offender.name, offender.ttfb_ms, max_ttfb
                ),
                logical(&offender.name, None),
            ));
        }
    }

    if crate::gates::all_lanes_failed(summaries) {
        for lane in summaries {
            results.push(result(
                "llmrace/lane-failure",
                &format!(
                    "{}: every round failed{}",
                    lane.name,
                    lane.error
                        .as_deref()
                        .map(|e| format!(" ({e})"))
                        .unwrap_or_default()
                ),
                logical(&lane.name, None),
            ));
        }
    }

    let log = json!({
        "version": "2.1.0",
        "$schema": "https://json.schemastore.org/sarif-2.1.0.json",
        "runs": [{
            "tool": {
                "driver": {
                    "name": "llmrace",
                    "version": env!("CARGO_PKG_VERSION"),
                    "informationUri": "https://github.com/dfbustosus/llmrace",
                    "rules": [
                        {
                            "id": "llmrace/regression",
                            "name": "MetricRegression",
                            "shortDescription": {
                                "text": "A metric regressed beyond the threshold against the baseline"
                            },
                        },
                        {
                            "id": "llmrace/request-mismatch",
                            "name": "RequestMismatch",
                            "shortDescription": {
                                "text": "The race is not the baseline's request; metrics were not compared"
                            },
                        },
                        {
                            "id": "llmrace/ttfb-gate",
                            "name": "TtfbGate",
                            "shortDescription": {
                                "text": "Mean time to first token exceeded the gate"
                            },
                        },
                        {
                            "id": "llmrace/lane-failure",
                            "name": "LaneFailure",
                            "shortDescription": {
                                "text": "Every measured round on this lane failed"
                            },
                        }
                    ]
                }
            },
            "results": results,
        }]
    });
    serde_json::to_string_pretty(&log).unwrap_or_else(|_| "{}".to_string())
}

fn result(rule: &str, message: &str, locations: Vec<Value>) -> Value {
    let mut result = json!({
        "ruleId": rule,
        "level": "error",
        "message": {"text": message},
    });
    if !locations.is_empty() {
        result["locations"] = Value::Array(vec![json!({"logicalLocations": locations})]);
    }
    result
}

fn logical(provider: &str, prompt_id: Option<&str>) -> Vec<Value> {
    let mut locations = vec![json!({"fullyQualifiedName": provider, "kind": "provider"})];
    if let Some(id) = prompt_id {
        locations.push(json!({"fullyQualifiedName": id, "kind": "prompt"}));
    }
    locations
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_green_race_emits_no_results() {
        let log: Value =
            serde_json::from_str(&sarif(&[], None, None)).expect("sarif renders valid json");
        assert_eq!(log["version"], "2.1.0");
        assert_eq!(
            log["runs"][0]["results"]
                .as_array()
                .expect("results is an array")
                .len(),
            0
        );
        assert_eq!(log["runs"][0]["tool"]["driver"]["name"], "llmrace");
    }
}
