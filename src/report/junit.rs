//! JUnit XML export for CI test-result ingestion.

use crate::bench::LaneSummary;

/// A JUnit XML report: one `<testsuite>` per lane and one `<testcase>` per
/// checked prompt (or per lane when the race had no task).
///
/// A case fails when its prompt's task did not pass every measured round,
/// or when the prompt had HTTP failures; a lane-level case fails when the
/// lane failed or dropped rounds. CI systems (GitHub, GitLab, Buildkite)
/// ingest JUnit natively, so a failing prompt shows up as a failing test.
#[must_use]
pub fn junit_xml(summaries: &[LaneSummary]) -> String {
    junit_xml_with_gate(summaries, None, false, false)
}

/// JUnit task cases plus baseline-gate failures, including missing evidence
/// when the caller explicitly requires it.
pub fn junit_xml_with_gate(
    summaries: &[LaneSummary],
    regression: Option<&crate::regression::RegressionReport>,
    require_significance: bool,
    fail_on_inconclusive: bool,
) -> String {
    struct Case {
        classname: String,
        name: String,
        time_s: f64,
        failure: Option<String>,
    }
    let mut suites: Vec<(String, Vec<Case>)> = Vec::new();
    for s in summaries {
        let classname = match &s.variant {
            Some(v) => format!("{}/{} ({v})", s.name, s.model),
            None => format!("{}/{}", s.name, s.model),
        };
        let suite_name = classname.clone();
        let mut cases: Vec<Case> = Vec::new();
        let rows = s.prompt_rows();
        if rows.is_empty() {
            let failure = if !s.successful() {
                Some(
                    s.error
                        .clone()
                        .unwrap_or_else(|| "no successful rounds".to_string()),
                )
            } else if s.failed_rounds() > 0 {
                Some(format!(
                    "{} of {} measured rounds failed",
                    s.failed_rounds(),
                    s.measured_rounds()
                ))
            } else {
                None
            };
            cases.push(Case {
                classname: classname.clone(),
                name: "completion".to_string(),
                time_s: s.mean_total_ms() / 1000.0,
                failure,
            });
        } else {
            for row in &rows {
                let http_failed = row.failed_rounds;
                let failure = match row.task_pass {
                    Some((ok, n)) if ok < n => {
                        Some(format!("task passed {ok}/{n} measured rounds"))
                    }
                    _ if http_failed > 0 => Some(format!(
                        "{http_failed} of this prompt's measured rounds failed"
                    )),
                    _ => None,
                };
                cases.push(Case {
                    classname: classname.clone(),
                    name: row.id.clone(),
                    time_s: row.total_ms / 1000.0,
                    failure,
                });
            }
        }
        suites.push((suite_name, cases));
    }
    if let Some(gate) = regression {
        let mut cases = Vec::new();
        if gate.request_mismatch {
            cases.push(Case {
                classname: "baseline".into(),
                name: "request identity".into(),
                time_s: 0.0,
                failure: Some("request or execution settings changed".into()),
            });
        }
        for comparison in &gate.comparisons {
            for metric in &comparison.metrics {
                if metric.fails_the_gate(require_significance)
                    || (fail_on_inconclusive && metric.inconclusive(require_significance))
                {
                    cases.push(Case {
                        classname: format!("{}/{}", comparison.name, comparison.current_model),
                        name: format!(
                            "{}: {}",
                            metric.prompt_id.as_deref().unwrap_or("lane"),
                            metric.label
                        ),
                        time_s: 0.0,
                        failure: Some(if metric.inconclusive(require_significance) {
                            "threshold breached without a usable significance test".into()
                        } else {
                            format!(
                                "{} changed from {} to {}",
                                metric.label, metric.baseline, metric.current
                            )
                        }),
                    });
                }
            }
        }
        if !cases.is_empty() {
            suites.push(("baseline gate".into(), cases));
        }
    }
    let total_tests: usize = suites.iter().map(|(_, c)| c.len()).sum();
    let total_failures: usize = suites
        .iter()
        .flat_map(|(_, c)| c.iter())
        .filter(|c| c.failure.is_some())
        .count();
    let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str(&format!(
        "<testsuites tests=\"{total_tests}\" failures=\"{total_failures}\">\n"
    ));
    for (name, cases) in &suites {
        let failures = cases.iter().filter(|c| c.failure.is_some()).count();
        let time: f64 = cases.iter().map(|c| c.time_s).sum();
        out.push_str(&format!(
            "  <testsuite name=\"{}\" tests=\"{}\" failures=\"{}\" time=\"{time:.3}\">\n",
            crate::util::xml_escape(name),
            cases.len(),
            failures
        ));
        for case in cases {
            out.push_str(&format!(
                "    <testcase classname=\"{}\" name=\"{}\" time=\"{:.3}\">\n",
                crate::util::xml_escape(&case.classname),
                crate::util::xml_escape(&case.name),
                case.time_s
            ));
            if let Some(message) = &case.failure {
                out.push_str(&format!(
                    "      <failure message=\"{}\"/>\n",
                    crate::util::xml_escape(message)
                ));
            }
            out.push_str("    </testcase>\n");
        }
        out.push_str("  </testsuite>\n");
    }
    out.push_str("</testsuites>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn junit_xml_strips_illegal_xml10_control_chars() {
        let failed = LaneSummary {
            name: "p\x00\x07".into(),
            model: "m'".into(),
            error: Some("bad \x1b[31merr".into()),
            ..LaneSummary::default()
        };
        let xml = junit_xml(&[failed]);
        assert!(!xml.contains('\x00') && !xml.contains('\x07') && !xml.contains('\x1b'));
        assert!(xml.contains("&apos;"), "{xml}");
    }
}
