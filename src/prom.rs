//! Prometheus pushgateway export of race metrics.

use crate::bench::LaneSummary;

/// Renders lane metrics in the Prometheus text exposition format.
///
/// Every series is labeled with `provider` and `model`; gauges cover TTFB,
/// mean/median/p95 latency, tokens/s, cost and tool-argument validity.
pub fn render_metrics(summaries: &[LaneSummary]) -> String {
    let mut out = String::new();
    for s in summaries.iter().filter(|s| s.successful()) {
        let labels = format!("provider=\"{}\",model=\"{}\"", s.name, escape(&s.model));
        out.push_str(&format!(
            "llmrace_ttfb_ms{{{labels}}} {:.3}\n",
            s.mean_ttfb_ms()
        ));
        out.push_str(&format!(
            "llmrace_latency_mean_ms{{{labels}}} {:.3}\n",
            s.mean_total_ms()
        ));
        out.push_str(&format!(
            "llmrace_latency_median_ms{{{labels}}} {:.3}\n",
            s.median_total_ms()
        ));
        out.push_str(&format!(
            "llmrace_latency_p95_ms{{{labels}}} {:.3}\n",
            s.p95_total_ms()
        ));
        out.push_str(&format!(
            "llmrace_tokens_per_second{{{labels}}} {:.3}\n",
            s.mean_tokens_per_sec()
        ));
        if let Some(cost) = s.cost_per_run_usd() {
            out.push_str(&format!(
                "llmrace_cost_per_run_usd{{{labels}}} {:.6}\n",
                cost
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
        .map_err(|e| crate::error::Error::Stream(format!("pushgateway unreachable: {e}")))?;
    let status = resp.status();
    if status.is_success() {
        Ok(())
    } else {
        let text = resp.text().await.unwrap_or_default();
        Err(crate::error::Error::Http {
            provider: "pushgateway".into(),
            status: status.as_u16(),
            body: text.chars().take(200).collect(),
        })
    }
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}
