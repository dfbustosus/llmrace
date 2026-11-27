//! Prometheus pushgateway export of race metrics.

use crate::bench::LaneSummary;

/// Renders lane metrics in the Prometheus text exposition format.
///
/// Every series is labeled with `provider` and `model`, plus `variant`
/// for sweep runs so lanes of different prompts never collide; gauges
/// cover TTFB, mean/median/p95 latency, tokens/s, cost and tool-argument
/// validity.
pub fn render_metrics(summaries: &[LaneSummary]) -> String {
    let mut out = String::new();
    for s in summaries {
        let labels = match &s.variant {
            Some(v) => format!(
                "provider=\"{}\",model=\"{}\",variant=\"{}\"",
                escape(&s.name),
                escape(&s.model),
                escape(v)
            ),
            None => format!(
                "provider=\"{}\",model=\"{}\"",
                escape(&s.name),
                escape(&s.model)
            ),
        };
        out.push_str(&format!(
            "llmrace_up{{{labels}}} {}\n",
            u8::from(s.successful())
        ));
        if let Some(rate) = s.failure_rate() {
            out.push_str(&format!(
                "llmrace_failure_rate{{{labels}}} {}\n",
                fmt_prom_float(rate, 6)
            ));
        }
        if !s.successful() {
            continue;
        }
        out.push_str(&format!(
            "llmrace_ttfb_ms{{{labels}}} {}\n",
            fmt_prom_float(s.mean_ttfb_ms(), 3)
        ));
        out.push_str(&format!(
            "llmrace_latency_mean_ms{{{labels}}} {}\n",
            fmt_prom_float(s.mean_total_ms(), 3)
        ));
        out.push_str(&format!(
            "llmrace_latency_median_ms{{{labels}}} {}\n",
            fmt_prom_float(s.median_total_ms(), 3)
        ));
        out.push_str(&format!(
            "llmrace_latency_p95_ms{{{labels}}} {}\n",
            fmt_prom_float(s.p95_total_ms(), 3)
        ));
        out.push_str(&format!(
            "llmrace_tokens_per_second{{{labels}}} {}\n",
            fmt_prom_float(s.mean_tokens_per_sec(), 3)
        ));
        out.push_str(&format!(
            "llmrace_goodput_tokens_per_second{{{labels}}} {}\n",
            fmt_prom_float(s.goodput_tokens_per_sec(), 3)
        ));
        out.push_str(&format!(
            "llmrace_retry_attempts{{{labels}}} {}\n",
            s.retry_attempts
        ));
        if let Some(cost) = s.cost_per_run_usd() {
            out.push_str(&format!(
                "llmrace_cost_per_run_usd{{{labels}}} {}\n",
                fmt_prom_float(cost, 6)
            ));
        }
        if let Some((valid, total)) = s.tool_validity() {
            out.push_str(&format!(
                "llmrace_tool_args_valid{{{labels}}} {valid}\nllmrace_tool_args_total{{{labels}}} {total}\n"
            ));
        }
    }
    out
}

/// Pushes the rendered metrics to a pushgateway job endpoint.
///
/// The URL should point at the full job path, e.g.
/// `https://push.example.com/metrics/job/llmrace` — labels appended by the
/// caller stay part of the path.
pub async fn push(
    client: &reqwest::Client,
    gateway_url: &str,
    summaries: &[LaneSummary],
) -> crate::error::Result<()> {
    let body = render_metrics(summaries);
    let resp = client
        .put(gateway_url)
        .header("content-type", "text/plain")
        .body(body)
        .send()
        .await
        .map_err(|e| {
            crate::error::Error::Stream(format!("pushgateway unreachable: {}", e.without_url()))
        })?;
    let status = resp.status();
    if status.is_success() {
        Ok(())
    } else {
        let text = crate::util::read_body_capped(resp, 16 * 1024)
            .await
            .unwrap_or_default();
        Err(crate::error::Error::Http {
            provider: "pushgateway".into(),
            status: status.as_u16(),
            body: crate::util::truncate(&text, 200),
            retry_after: None,
        })
    }
}

fn fmt_prom_float(v: f64, decimals: usize) -> String {
    if v.is_nan() {
        "NaN".to_string()
    } else if v.is_infinite() {
        if v.is_sign_positive() {
            "+Inf".to_string()
        } else {
            "-Inf".to_string()
        }
    } else {
        format!("{v:.decimals$}")
    }
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prom_float_formats_non_finite_values_per_exposition_spec() {
        assert_eq!(fmt_prom_float(f64::INFINITY, 3), "+Inf");
        assert_eq!(fmt_prom_float(f64::NEG_INFINITY, 3), "-Inf");
        assert_eq!(fmt_prom_float(f64::NAN, 3), "NaN");
        assert_eq!(fmt_prom_float(12.3456, 3), "12.346");
        assert_eq!(fmt_prom_float(0.125, 6), "0.125000");
    }

    #[test]
    fn escape_handles_quotes_backslashes_and_crlf() {
        assert_eq!(escape("a\"b\\c\r\nd"), "a\\\"b\\\\c\\r\\nd");
    }
}
