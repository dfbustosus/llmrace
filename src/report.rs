//! Report generation: markdown, JSON and CSV exports of race results.

use crate::bench::LaneSummary;
use crate::cost;
use crate::stats::fmt_ms;

/// Renders a shareable markdown report ranked by aggregate tokens/s.
///
/// When lanes carry variant labels (a `--sweep` run), one table is emitted
/// per variant.
/// Renders just the significance verdict line, if computable.
pub(crate) fn significance_line(summaries: &[LaneSummary]) -> Option<String> {
    top_two_significance(summaries).map(|(a, b, delta, p, significant)| {
        if significant {
            format!(
                "*Significance*: {a} vs {b} — \u{394} {delta:+.0} ms, statistically significant (p = {p:.3}) (Welch's t-test)."
            )
        } else {
            format!(
                "*Significance*: {a} vs {b} — \u{394} {delta:+.0} ms, **not** statistically significant (p = {p:.2}) — the gap could be noise (Welch's t-test)."
            )
        }
    })
}

/// Renders a shareable markdown report ranked by aggregate tokens/s.
///
/// When lanes carry variant labels (a `--sweep` run), one table is emitted
/// per variant.
pub fn markdown(summaries: &[LaneSummary], prompt: &str, runs: usize, max_tokens: u64) -> String {
    let mut out = String::new();
    out.push_str("# 🏁 llmrace report\n\n");
    out.push_str(&format!(
        "- **Prompt**: `\"{}\"`\n",
        prompt.replace('\n', " ")
    ));
    out.push_str(&format!("- **Measured rounds per provider**: {}\n", runs));
    out.push_str(&format!("- **Max tokens**: {}\n", max_tokens));
    out.push_str(&format!("- **Generated**: {}\n", today_utc()));

    for (variant, lanes) in group_by_variant(summaries) {
        if let Some(name) = &variant {
            let desc = crate::variants::builtins()
                .into_iter()
                .find(|v| v.name == *name)
                .map(|v| v.description)
                .unwrap_or("custom variant (llmrace.toml)");
            out.push_str(&format!("\n## variant: `{name}` — {desc}\n"));
        }
        out.push_str(&variant_table(&lanes));
    }

    if let Some((a, b, delta, p, significant)) = top_two_significance(summaries) {
        let verdict = if significant {
            format!("statistically significant (p = {p:.3})")
        } else {
            format!("**not** statistically significant (p = {p:.2}) — the gap could be noise")
        };
        out.push_str(&format!(
            "\n*Significance*: {a} vs {b} — Δ {delta:+.0} ms mean latency, {verdict} (Welch's t-test on per-run latencies).\n"
        ));
    }

    let unpriced: Vec<&LaneSummary> = summaries
        .iter()
        .filter(|s| s.successful() && s.cost_per_run_usd().is_none())
        .collect();
    if !unpriced.is_empty() {
        out.push_str(&format!(
            "\n*no pricing data for {} lane(s) — cost column empty; pass `--price provider/model=in:out`*\n",
            unpriced.len()
        ));
    }

    let failed: Vec<&LaneSummary> = summaries.iter().filter(|s| !s.successful()).collect();
    if !failed.is_empty() {
        out.push_str("\n**Errors**\n\n");
        for s in failed {
            let variant = s
                .variant
                .as_deref()
                .map(|v| format!(" ({v})"))
                .unwrap_or_default();
            out.push_str(&format!(
                "- `{}{}` — {}\n",
                s.name,
                variant,
                s.error.as_deref().unwrap_or("no successful rounds")
            ));
        }
    }

    if summaries.iter().any(|s| !s.outlier_runs().is_empty()) {
        out.push_str("\n† at least one round was a 3-sigma latency outlier\n");
    }

    out.push_str(&prompt_table(summaries));
    out.push_str("\n> TTFT is time to first token, the same clock as TTFB. Tokens/s is measured over the generation window (total time minus TTFT), summed across concurrent requests per round. TPOT is that window divided by output tokens; the JSON report gives its median and p95. Token counts use provider-reported usage when available. Latency, tokens/s and cost/run use successful rounds only.\n");
    if summaries.iter().any(|s| !s.failures.is_empty()) {
        out.push_str("> Fail is failed/measured rounds. Rank is goodput: tokens/s × the success rate, so a lane that dropped rounds does not outrank one that answered every round.\n");
    }
    if summaries.iter().any(|s| s.task_pass().is_some()) {
        out.push_str("> Pass is task successes over measured rounds. A fast wrong answer is still a successful request. `--winner` counts it; a race with no task leaves the column blank and the podium unchanged.\n");
    }
    if summaries.iter().any(|s| s.retry_attempts > 0) {
        out.push_str("> Retries are extra attempts before a round settled. They are counted and not priced: a failed attempt does not return token usage.\n");
    }
    out.push_str("> Cost/run uses billed input and output tokens when the provider reported them. Cache reads and cache writes use the published discount when this crate has one for the model; hourly cache storage is not in the per-request usage and is not included. A trailing ~ means input tokens were estimated from the user prompt, which leaves out a system prompt and tool schemas. ");
    out.push_str(&crate::cost::pricing_stamp());
    out.push('\n');
    let undiscounted: Vec<&str> = summaries
        .iter()
        .filter(|s| s.successful() && !s.cache_discount_applied())
        .map(|s| s.name.as_str())
        .collect();
    if !undiscounted.is_empty() {
        out.push_str(&format!(
            "\n*cache tokens on {} were billed at the input rate — no cached-input price is published for that model; pass `--price provider/model=in:out:cached`*\n",
            undiscounted.join(", ")
        ));
    }
    out.push_str("> Generated with [llmrace](https://github.com/dfbustosus/llmrace) 🦀\n");
    out
}

fn group_by_variant(summaries: &[LaneSummary]) -> Vec<(Option<String>, Vec<&LaneSummary>)> {
    let mut order: Vec<Option<String>> = Vec::new();
    for s in summaries {
        if !order.contains(&s.variant) {
            order.push(s.variant.clone());
        }
    }
    order
        .into_iter()
        .map(|v| {
            (
                v.clone(),
                summaries.iter().filter(|s| s.variant == v).collect(),
            )
        })
        .collect()
}

pub(crate) fn variant_table(lanes: &[&LaneSummary]) -> String {
    let ranked = rank(lanes);
    let mut out = String::new();
    out.push_str("\n| # | Provider | Model | C | Latency (mean ± 95% CI) | p50 | p95 | TTFT | Server | Network | ITL p95 | Stutters | Tokens/s | Pass | Fail | Cost/run |\n");
    out.push_str("|--|----------|-------|--:|------------------------:|----:|-----:|-----:|-------:|--------:|--------:|---------:|---------:|-----:|---------:|---------:|\n");
    for (i, s) in ranked.iter().enumerate() {
        let mean = match s.ci95_total_ms() {
            Some((lo, hi)) => format!("{:.0} ± {:.0} ms", s.mean_total_ms(), (hi - lo) / 2.0),
            None => format!("{:.0} ms", s.mean_total_ms()),
        };
        let cost = match (s.cost_per_run_usd(), s.prompt_tokens_measured()) {
            (Some(v), true) => cost::format_usd(v),
            (Some(v), false) => format!("{}~", cost::format_usd(v)),
            (None, _) => "—".to_string(),
        };
        let outlier_mark = if s.outlier_runs().is_empty() {
            ""
        } else {
            "†"
        };
        let itl_p95 = s
            .itl_p95_ms()
            .map(|v| format!("{v:.0} ms"))
            .unwrap_or_else(|| "—".into());
        let fail = format!("{}/{}", s.failures.len(), s.measured_rounds());
        let pass = match s.task_pass() {
            Some((ok, n)) => format!("{ok}/{n}"),
            None => "—".to_string(),
        };
        out.push_str(&format!(
            "| {} | {}{} | `{}` | {} | {} | {:.0} ms | {:.0} ms | {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
            i + 1,
            s.name,
            outlier_mark,
            s.model,
            s.concurrency,
            mean,
            s.median_total_ms(),
            s.p95_total_ms(),
            fmt_ms(s.mean_ttfb_ms()),
            s.median_server_ms()
                .map(|v| format!("{v:.0} ms"))
                .unwrap_or_else(|| "—".into()),
            s.network_overhead_ms()
                .map(|v| format!("{v:+.0} ms"))
                .unwrap_or_else(|| "—".into()),
            itl_p95,
            if s.stutters() > 0 {
                format!("{} ⏸", s.stutters())
            } else {
                "0".to_string()
            },
            if s.reasoning_tokens() > 0 {
                format!("**{:.1}** 🧠{}", s.mean_tokens_per_sec(), s.reasoning_tokens())
            } else {
                format!("**{:.1}**", s.mean_tokens_per_sec())
            },
            pass,
            fail,
            cost
        ));
    }
    out
}

/// Per-prompt rows, so a slow prompt is visible. Empty when the race
/// used one prompt.
fn prompt_table(summaries: &[LaneSummary]) -> String {
    if summaries.iter().all(|s| s.prompt_rows().is_empty()) {
        return String::new();
    }
    let mut out = String::from(
        "\n## Per prompt\n\n| Provider | Prompt | Rounds | Pass | TTFT | Tokens/s |\n|----------|--------|-------:|-----:|-----:|---------:|\n",
    );
    for s in summaries {
        // same-provider multi-model races name the model, mirroring the
        // significance labels
        let dup = summaries.iter().filter(|x| x.name == s.name).count() > 1;
        let provider = if dup && !s.model.is_empty() {
            format!("{}/{}", s.name, s.model)
        } else {
            s.name.clone()
        };
        for row in s.prompt_rows() {
            let pass = match row.task_pass {
                Some((ok, n)) => format!("{ok}/{n}"),
                None => "—".into(),
            };
            out.push_str(&format!(
                "| {} | `{}` | {} | {pass} | {:.0} ms | {:.1} |\n",
                provider, row.id, row.rounds, row.ttfb_ms, row.tokens_per_sec
            ));
        }
    }
    out
}

/// Lanes ranked best-first by goodput (tokens/s × success rate).
///
/// Fully failed lanes drop out. With no failed rounds, goodput equals
/// tokens/s, so a clean race keeps the previous order.
fn rank<L: std::borrow::Borrow<LaneSummary>>(lanes: &[L]) -> Vec<&LaneSummary> {
    let mut ranked: Vec<&LaneSummary> = lanes
        .iter()
        .filter(|s| s.borrow().successful())
        .map(|s| s.borrow())
        .collect();
    ranked.sort_by(|a, b| {
        b.goodput_tokens_per_sec()
            .partial_cmp(&a.goodput_tokens_per_sec())
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                a.failure_rate()
                    .unwrap_or(0.0)
                    .partial_cmp(&b.failure_rate().unwrap_or(0.0))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });
    ranked
}

/// One-line description of the sampling controls that were actually sent.
///
/// `None` when every control was left at the provider default, so reports
/// for races that do not pin sampling stay identical to earlier versions.
#[must_use]
pub fn sampling_inline(sampling: &crate::provider::Sampling) -> Option<String> {
    if sampling.is_unset() {
        return None;
    }
    let mut parts = Vec::new();
    if let Some(t) = sampling.temperature {
        parts.push(format!("temperature {t}"));
    }
    if let Some(p) = sampling.top_p {
        parts.push(format!("top_p {p}"));
    }
    if let Some(seed) = sampling.seed {
        parts.push(format!("seed {seed}"));
    }
    if sampling.system.as_deref().is_some_and(|s| !s.is_empty()) {
        parts.push("system prompt".to_string());
    }
    if let Some(json_output) = &sampling.json_output {
        parts.push(match json_output {
            crate::provider::JsonOutput::Object => "json output".to_string(),
            crate::provider::JsonOutput::Schema(_) => "json schema".to_string(),
        });
    }
    Some(parts.join(", "))
}

/// Markdown metadata line for [`sampling_inline`].
#[must_use]
pub fn sampling_note(sampling: &crate::provider::Sampling) -> Option<String> {
    sampling_inline(sampling).map(|s| format!("- **Sampling**: {s}\n"))
}

/// Inserts a metadata note above the Generated line, or appends it when
/// the report has none. The shared way [`sampling_note`] and
/// [`cache_note`] reach a report.
pub fn insert_note(md: &mut String, note: &str) {
    if let Some(pos) = md.find("- **Generated**:") {
        md.insert_str(pos, note);
    } else {
        md.push_str(note);
    }
}

/// A one-line markdown note naming the lanes that mix cold and warm
/// rounds. `None` when every lane is uniformly cold or uniformly warm.
#[must_use]
pub fn cache_note(summaries: &[LaneSummary]) -> Option<String> {
    let mixed: Vec<String> = summaries
        .iter()
        .filter(|s| {
            let hits = s.cache_hit_rounds();
            !s.runs.is_empty() && hits > 0 && hits < s.runs.len()
        })
        .map(|s| format!("{} ({}/{})", s.name, s.cache_hit_rounds(), s.runs.len()))
        .collect();
    if mixed.is_empty() {
        None
    } else {
        Some(format!(
            "- **Cache mix**: {} — some rounds hit the prefix cache and some did not, so the latency sample is not homogeneous\n",
            mixed.join(", ")
        ))
    }
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

/// Inserts a `sampling` object into an already-rendered JSON report.
///
/// Returns the input unchanged when sampling is unset or the document is
/// not an object, so older call sites keep their bytes.
#[must_use]
pub fn insert_sampling_json(json: &str, sampling: &crate::provider::Sampling) -> String {
    let Some(value) = sampling_json(sampling) else {
        return json.to_string();
    };
    let Ok(mut doc) = serde_json::from_str::<serde_json::Value>(json) else {
        return json.to_string();
    };
    if let Some(map) = doc.as_object_mut() {
        map.insert("sampling".to_string(), value);
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
        map.insert("regression".to_string(), verdict.clone());
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
    json_report_with_region(summaries, prompt, runs, max_tokens, None)
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
    render_json(
        summaries,
        prompt,
        runs,
        max_tokens,
        region,
        &SentPrompts {
            prompt,
            workload: &[],
            warmup: 0,
        },
        &request_sha256(
            prompt,
            &[],
            &crate::provider::Sampling::default(),
            max_tokens,
            None,
        ),
    )
}

/// The prompts a race sent, so `race.json` can name them.
pub struct SentPrompts<'a> {
    /// Text a one-string race sent. A workload replaces it, round by round.
    pub prompt: &'a str,
    /// Workload drawn in measured-round order. Empty for a one-string race.
    pub workload: &'a [crate::workload::WorkloadPrompt],
    /// Warmup rounds. Measured rounds start after these, matching the engine.
    pub warmup: usize,
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
    render_json(
        summaries,
        prompt,
        runs,
        max_tokens,
        region,
        sent,
        request_sha256,
    )
}

/// Hex SHA-256 of the request a race made: the prompt or workload (including
/// `expect`), `max_tokens`, sampling, the system prompt, and the tool task.
///
/// `tools` is the task name when `--tools` is set, and absent otherwise.
/// Unset sampling fields are empty. Round count is not part of the hash:
/// a longer sample of the same request stays the same request.
#[must_use]
pub fn request_sha256(
    sent_prompt: &str,
    workload: &[crate::workload::WorkloadPrompt],
    sampling: &crate::provider::Sampling,
    max_tokens: u64,
    tools: Option<&str>,
) -> String {
    let mut body = String::from("llmrace-request-v1\n");
    push_field(&mut body, "max_tokens", &max_tokens.to_string());
    push_field(&mut body, "temperature", &fmt_opt_f64(sampling.temperature));
    push_field(&mut body, "top_p", &fmt_opt_f64(sampling.top_p));
    push_field(
        &mut body,
        "seed",
        &sampling.seed.map(|s| s.to_string()).unwrap_or_default(),
    );
    push_field(
        &mut body,
        "system",
        sampling.system.as_deref().unwrap_or(""),
    );
    if let Some(json_output) = &sampling.json_output {
        let value = match json_output {
            crate::provider::JsonOutput::Object => "object".to_string(),
            crate::provider::JsonOutput::Schema(schema) => schema.to_string(),
        };
        push_field(&mut body, "json_output", &value);
    }
    push_field(&mut body, "tools", tools.unwrap_or(""));
    if workload.is_empty() {
        push_field(&mut body, "prompt", sent_prompt);
        push_field(&mut body, "workload", "");
    } else {
        push_field(&mut body, "prompt", "");
        let mut blob = String::new();
        for item in workload {
            push_field(&mut blob, "id", &item.id);
            push_field(&mut blob, "prompt", &item.prompt);
            push_field(&mut blob, "expect", item.expect.as_deref().unwrap_or(""));
            if item.expect_match != crate::workload::ExpectMatch::Exact {
                push_field(&mut blob, "match", item.expect_match.as_str());
            }
            if let Some(system) = &item.system {
                push_field(&mut blob, "system", system);
            }
            if !item.messages.is_empty() {
                let mut turns = String::new();
                for message in &item.messages {
                    push_field(&mut turns, "role", message.role.as_str());
                    push_field(&mut turns, "content", &message.content);
                }
                push_field(&mut blob, "messages", &turns);
            }
            if let Some(tool) = &item.tool {
                push_field(&mut blob, "tool", &tool.name);
                push_field(&mut blob, "tool_description", &tool.description);
                push_field(&mut blob, "tool_parameters", &tool.parameters.to_string());
            }
        }
        push_field(&mut body, "workload", &blob);
    }
    crate::provider::sha256_hex(body.as_bytes())
}

fn push_field(out: &mut String, name: &str, value: &str) {
    out.push_str(name);
    out.push(' ');
    out.push_str(&value.len().to_string());
    out.push('\n');
    out.push_str(value);
    out.push('\n');
}

fn fmt_opt_f64(value: Option<f64>) -> String {
    value.map(|v| format!("{v:?}")).unwrap_or_default()
}

/// Prompt ids in measured-round order, and the SHA-256 of the prompt
/// texts those rounds sent, joined with a newline.
fn citation(sent: &SentPrompts<'_>, rounds: usize) -> (Vec<String>, String) {
    let mut ids = Vec::with_capacity(rounds);
    let mut texts = Vec::with_capacity(rounds);
    for i in 0..rounds {
        let (id, text) =
            crate::workload::prompt_for(sent.prompt, sent.workload, sent.warmup, sent.warmup + i);
        if !sent.workload.is_empty() {
            ids.push(id.to_string());
        }
        texts.push(text.to_string());
    }
    let hash = crate::provider::sha256_hex(texts.join("\n").as_bytes());
    (ids, hash)
}

fn counted_rounds(summaries: &[LaneSummary], fallback: usize) -> usize {
    let n = summaries
        .iter()
        .map(|s| {
            let concurrent = s.concurrency.max(1);
            s.runs.len().div_ceil(concurrent) + s.failures.len()
        })
        .max()
        .unwrap_or(0);
    if n == 0 {
        fallback
    } else {
        n
    }
}

fn render_json(
    summaries: &[LaneSummary],
    prompt: &str,
    runs: usize,
    max_tokens: u64,
    region: Option<&str>,
    sent: &SentPrompts<'_>,
    request_sha256: &str,
) -> String {
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
                "failed_rounds": s.failures.len(),
                "failure_rate": s.failure_rate(),
                "retry_attempts": s.retry_attempts,
                "goodput_tokens_per_sec": s.goodput_tokens_per_sec(),
                "failures": s.failures.iter().map(|f| serde_json::json!({
                    "elapsed_ms": f.elapsed_ms,
                    "class": f.class.as_str(),
                    "attempts": f.attempts,
                    "message": f.message,
                })).collect::<Vec<_>>(),
                "runs": s.runs.iter().map(|r| {
                    let mut run = serde_json::json!({
                        "total_ms": r.total_ms,
                        "ttfb_ms": r.ttfb_ms,
                        "tokens": r.tokens,
                        "chars": r.chars,
                        "prompt_tokens": r.prompt_tokens,
                        "cache_read_tokens": r.cache_read_tokens,
                        "cache_write_tokens": r.cache_write_tokens,
                        "cache_write_1h_tokens": r.cache_write_1h_tokens,
                        "tokens_per_sec": r.tokens_per_sec(),
                    });
                    if !r.prompt_id.is_empty() {
                        run["prompt_id"] = serde_json::json!(r.prompt_id);
                    }
                    run
                }).collect::<Vec<_>>(),
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
            let prompts: Vec<serde_json::Value> = s
                .prompt_rows()
                .into_iter()
                .map(|row| {
                    serde_json::json!({
                        "id": row.id,
                        "rounds": row.rounds,
                        "mean_total_ms": row.total_ms,
                        "mean_ttfb_ms": row.ttfb_ms,
                        "mean_tokens_per_sec": row.tokens_per_sec,
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
    serde_json::to_string_pretty(&serde_json::json!({
        "tool": "llmrace",
        "version": env!("CARGO_PKG_VERSION"),
        "region": region,
        "prompt": prompt,
        "pricing_as_of": crate::cost::PRICING_AS_OF,
        "workload": workload_ids,
        "prompt_sha256": prompt_sha256,
        "request_sha256": request_sha256,
        "runs": runs,
        "max_tokens": max_tokens,
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
    }))
    .unwrap_or_default()
}

/// A JUnit XML report: one `<testsuite>` per lane and one `<testcase>` per
/// checked prompt (or per lane when the race had no task).
///
/// A case fails when its prompt's task did not pass every measured round,
/// or when the prompt had HTTP failures; a lane-level case fails when the
/// lane failed or dropped rounds. CI systems (GitHub, GitLab, Buildkite)
/// ingest JUnit natively, so a failing prompt shows up as a failing test.
#[must_use]
pub fn junit_xml(summaries: &[LaneSummary]) -> String {
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
            } else if !s.failures.is_empty() {
                Some(format!(
                    "{} of {} measured rounds failed",
                    s.failures.len(),
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
                let http_failed = s.failures.iter().filter(|f| f.prompt_id == row.id).count();
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
            crate::util::html_escape(name),
            cases.len(),
            failures
        ));
        for case in cases {
            out.push_str(&format!(
                "    <testcase classname=\"{}\" name=\"{}\" time=\"{:.3}\">\n",
                crate::util::html_escape(&case.classname),
                crate::util::html_escape(&case.name),
                case.time_s
            ));
            if let Some(message) = &case.failure {
                out.push_str(&format!(
                    "      <failure message=\"{}\"/>\n",
                    crate::util::html_escape(message)
                ));
            }
            out.push_str("    </testcase>\n");
        }
        out.push_str("  </testsuite>\n");
    }
    out.push_str("</testsuites>\n");
    out
}

/// Renders per-run rows as CSV.
pub fn csv(summaries: &[LaneSummary]) -> String {
    let mut out = String::from("provider,model,run,total_ms,ttfb_ms,tokens,tokens_per_sec\n");
    for s in summaries {
        for (i, r) in s.runs.iter().enumerate() {
            out.push_str(&format!(
                "{},{},{},{:.3},{:.3},{},{:.3}\n",
                crate::util::csv_field(&s.name),
                crate::util::csv_field(&s.model),
                i + 1,
                r.total_ms,
                r.ttfb_ms,
                r.tokens,
                r.tokens_per_sec()
            ));
        }
    }
    out
}

/// Welch significance between the top two successful lanes of the same
/// variant group: `(leader, runner_up, delta_ms, p, significant)`.
///
/// `None` when fewer than two comparable lanes or samples exist — the
/// ranking then carries no statistical claim.
fn top_two_significance(summaries: &[LaneSummary]) -> Option<(String, String, f64, f64, bool)> {
    let ranked = rank(summaries);
    // compare within the leader's variant group: different prompts must
    // not be statistically compared
    let leader = ranked.first()?;
    let runner_up = ranked
        .iter()
        .skip(1)
        .find(|s| s.variant == leader.variant && s.runs.len() >= 2)?;
    if leader.runs.len() < 2 {
        return None;
    }
    let a: Vec<f64> = leader.runs.iter().map(|r| r.total_ms).collect();
    let b: Vec<f64> = runner_up.runs.iter().map(|r| r.total_ms).collect();
    let (t, df) = crate::stats::welch_t(&a, &b)?;
    let p = crate::stats::t_two_sided_p(t, df);
    let delta = leader.mean_total_ms() - runner_up.mean_total_ms();
    // disambiguate same-provider multi-model races, mirroring the podium
    let label = |s: &LaneSummary| {
        let dup = summaries
            .iter()
            .filter(|x| x.name == s.name && x.successful())
            .count()
            > 1;
        if dup && !s.model.is_empty() {
            format!("{}/{}", s.name, s.model)
        } else {
            s.name.clone()
        }
    };
    Some((label(leader), label(runner_up), delta, p, p < 0.05))
}

/// Renders the compact terminal ranking table.
pub fn terminal_table(summaries: &[LaneSummary]) -> String {
    let ranked = rank(summaries);
    let any_task = summaries.iter().any(|s| s.task_pass().is_some());
    // one header/row shape, with the Pass column spliced in when the race
    // had a task
    let task_head = if any_task {
        format!(" {:>5} {:>5}", "Fail", "Pass")
    } else {
        format!(" {:>5}", "Fail")
    };
    let mut out = format!(
        "{:<4} {:<14} {:<24} {:>3} {:>16} {:>7} {:>8} {:>7} {:>8} {:>8}{} {:>7}\n",
        "#",
        "Provider",
        "Model",
        "C",
        "Lat (mean±CI)",
        "p95",
        "TTFT",
        "Server",
        "Network",
        "ITLp95",
        task_head,
        "Tok/s"
    );
    out.push_str(&"-".repeat(out.len()));
    out.push('\n');
    for (i, s) in ranked.iter().enumerate() {
        let mean = match s.ci95_total_ms() {
            Some((lo, hi)) => format!("{:.0}±{:.0}ms", s.mean_total_ms(), (hi - lo) / 2.0),
            None => format!("{:.0}ms", s.mean_total_ms()),
        };
        let mark = if s.outlier_runs().is_empty() {
            ""
        } else {
            "†"
        };
        let itl = s
            .itl_p95_ms()
            .map(|v| {
                let st = if s.stutters() > 0 {
                    format!("⏸{}", s.stutters())
                } else {
                    String::new()
                };
                format!("{v:.0}{st}")
            })
            .unwrap_or_else(|| "—".into());
        let reasoning = if s.reasoning_tokens() > 0 && crate::provider::is_reasoning_model(&s.model)
        {
            format!(" 🧠{}", s.reasoning_tokens())
        } else {
            String::new()
        };
        let fail = format!("{}/{}", s.failures.len(), s.measured_rounds());
        let fail_cols = if any_task {
            let pass = s
                .task_pass()
                .map(|(ok, n)| format!("{ok}/{n}"))
                .unwrap_or_else(|| "—".into());
            format!(" {:>5} {:>5}", fail, pass)
        } else {
            format!(" {:>5}", fail)
        };
        out.push_str(&format!(
            "{:<4} {:<14} {:<24} {:>3} {:>16} {:>6.0}ms {:>6}ms {:>7} {:>8} {:>8}{} {:>8.1}{}{}\n",
            i + 1,
            s.name,
            crate::util::truncate(&s.model, 26),
            s.concurrency,
            mean,
            s.p95_total_ms(),
            fmt_ms(s.mean_ttfb_ms()),
            s.median_server_ms()
                .map(crate::stats::fmt_ms)
                .unwrap_or_else(|| "—".into()),
            s.network_overhead_ms()
                .map(|v| format!("{v:+.0}"))
                .unwrap_or_else(|| "—".into()),
            itl,
            fail_cols,
            s.mean_tokens_per_sec(),
            reasoning,
            mark
        ));
    }
    for s in summaries.iter().filter(|s| !s.successful()) {
        out.push_str(&format!(
            "  ✗ {} — {}\n",
            s.name,
            s.error.as_deref().unwrap_or("no successful rounds")
        ));
    }
    if let Some((a, b, delta, p, significant)) = top_two_significance(summaries) {
        let verdict = if significant {
            format!("significant (p={p:.3})")
        } else {
            format!("NOT significant (p={p:.2}) — the gap could be noise")
        };
        out.push_str(&format!("⚖ {a} vs {b}: Δ {delta:+.0} ms, {verdict}\n"));
    }
    out
}

fn today_utc() -> String {
    format!("{} UTC", crate::util::today_utc())
}

/// Renders the latency distribution section: percentile ladder plus a
/// block-bar histogram per successful provider.
///
/// Providers need at least three measured rounds to be rendered.
pub fn histogram_section(summaries: &[LaneSummary]) -> String {
    const BUCKETS: usize = 12;
    const MAX_BAR: usize = 30;

    let lanes: Vec<&LaneSummary> = summaries
        .iter()
        .filter(|s| s.successful() && s.runs.len() >= 3)
        .collect();
    if lanes.is_empty() {
        return String::new();
    }

    let mut out = String::from("\n## Latency distribution\n\n");
    out.push_str("| Provider | p10 | p25 | p50 | p75 | p90 | p95 | p99 |\n");
    out.push_str("|--|--:|--:|--:|--:|--:|--:|--:|--:|\n");
    for s in &lanes {
        let cells: Vec<String> = s
            .latency_percentiles()
            .iter()
            .map(|(_, v)| format!("{v:.0} ms"))
            .collect();
        out.push_str(&format!("| {} | {} |\n", s.name, cells.join(" | ")));
    }

    // TTFB ladder: first-token latency distribution, distinct from the
    // time-to-last-token ladder above
    let ps = [10.0, 25.0, 50.0, 75.0, 90.0, 95.0, 99.0];
    out.push_str("\n## TTFB distribution (TTFT)\n\n");
    out.push_str("| Provider | p10 | p25 | p50 | p75 | p90 | p95 | p99 |\n");
    out.push_str("|--|--:|--:|--:|--:|--:|--:|--:|\n");
    for s in &lanes {
        let sample = crate::stats::Sample::from_values(s.runs.iter().map(|r| r.ttfb_ms).collect());
        let cells: Vec<String> = sample
            .percentiles(&ps)
            .iter()
            .map(|v| format!("{v:.0} ms"))
            .collect();
        out.push_str(&format!("| {} | {} |\n", s.name, cells.join(" | ")));
    }

    out.push_str("\n```\n");
    for s in &lanes {
        let values: Vec<f64> = s.runs.iter().map(|r| r.total_ms).collect();
        let upper = s
            .latency_percentiles()
            .iter()
            .find(|(l, _)| *l == "p99")
            .map(|(_, v)| *v)
            .unwrap_or(0.0);
        let scale = if upper > 0.0 { upper } else { 1.0 };
        let width = scale / BUCKETS as f64;
        let mut histogram = [0usize; BUCKETS];
        for v in &values {
            let idx = ((v / scale) * (BUCKETS - 1) as f64).round() as usize;
            histogram[idx.min(BUCKETS - 1)] += 1;
        }
        let peak = histogram.iter().copied().max().unwrap_or(1).max(1);
        out.push_str(&format!("{}\n", s.name));
        for (i, count) in histogram.iter().enumerate() {
            let bar_len = (count * MAX_BAR / peak).min(MAX_BAR);
            let bar: String = "▇".repeat(bar_len);
            out.push_str(&format!(
                "  {:>6.0}–{:<6.0} ms │{:<width$} {}\n",
                i as f64 * width,
                (i + 1) as f64 * width,
                bar,
                count,
                width = MAX_BAR
            ));
        }
    }
    out.push_str("```\n");
    out
}

/// Renders the race as a self-contained dark-theme HTML page — podium,
/// ranked table and per-run latency bars — ready to share or host.
///
/// `scored` carries the composite podium lanes (from `--winner`); pass an
/// empty slice to omit the section.
pub fn html_report(
    summaries: &[LaneSummary],
    prompt: &str,
    runs: usize,
    max_tokens: u64,
    scored: &[crate::score::Scored],
) -> String {
    let groups: Vec<(Option<String>, Vec<crate::score::Scored>)> = if scored.is_empty() {
        Vec::new()
    } else {
        vec![(None, scored.to_vec())]
    };
    html_report_grouped(summaries, prompt, runs, max_tokens, &groups)
}

/// Like [`html_report`], but with one podium per prompt-variant group —
/// sweep runs must not mix variants in a single podium.
pub fn html_report_grouped(
    summaries: &[LaneSummary],
    prompt: &str,
    runs: usize,
    max_tokens: u64,
    groups: &[(Option<String>, Vec<crate::score::Scored>)],
) -> String {
    let mut body = String::new();

    for (variant, scored) in groups {
        if scored.is_empty() {
            continue;
        }
        let medals = ["🥇", "🥈", "🥉"];
        let heading = match variant {
            Some(v) => format!("Podium · {}", crate::util::html_escape(v)),
            None => "Podium".to_string(),
        };
        body.push_str(&format!("<section><h2>{heading}</h2><ol class='podium'>"));
        for (i, s) in scored.iter().enumerate() {
            let medal = medals.get(i).copied().unwrap_or("·");
            body.push_str(&format!(
                "<li><span class='medal'>{medal}</span> {} <span class='score'>{:.1}</span></li>",
                crate::util::html_escape(&s.name),
                s.score
            ));
        }
        body.push_str("</ol></section>");
    }

    body.push_str("<section><h2>Results</h2><table><tr><th>#</th><th>Provider</th><th>Model</th><th>C</th><th>Latency (mean ± 95% CI)</th><th>p95</th><th>TTFT</th><th>ITL p95</th><th>Pass</th><th>Fail</th><th>Tokens/s</th></tr>");
    for (i, s) in rank(summaries).iter().enumerate() {
        let ci = match s.ci95_total_ms() {
            Some((lo, hi)) => format!("±{:.0}", (hi - lo) / 2.0),
            None => "—".to_string(),
        };
        body.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{:.0} ms {}</td><td>{:.0} ms</td><td>{}</td><td>{}</td><td>{}</td><td>{}/{}</td><td><b>{:.1}</b></td></tr>",
            i + 1,
            crate::util::html_escape(&s.name),
            crate::util::html_escape(&s.model),
            s.concurrency,
            s.mean_total_ms(),
            ci,
            s.p95_total_ms(),
            fmt_ms(s.mean_ttfb_ms()),
            s.itl_p95_ms().map(|v| format!("{v:.0} ms")).unwrap_or_else(|| "—".into()),
            match s.task_pass() {
                Some((ok, n)) => format!("{ok}/{n}"),
                None => "—".to_string(),
            },
            s.failures.len(),
            s.measured_rounds(),
            s.mean_tokens_per_sec(),
        ));
    }
    body.push_str("</table></section>");

    let lanes: Vec<&LaneSummary> = summaries.iter().filter(|s| s.successful()).collect();
    if !lanes.is_empty() {
        body.push_str("<section><h2>Per-run latency</h2>");
        for s in &lanes {
            body.push_str(&lane_bars(s));
        }
        body.push_str("</section>");
    }

    let failures: Vec<&LaneSummary> = summaries.iter().filter(|s| !s.successful()).collect();
    if !failures.is_empty() {
        body.push_str("<section><h2>Errors</h2><ul>");
        for s in &failures {
            body.push_str(&format!(
                "<li>{} — {}</li>",
                crate::util::html_escape(&s.name),
                crate::util::html_escape(s.error.as_deref().unwrap_or("no successful rounds"))
            ));
        }
        body.push_str("</ul></section>");
    }

    let label: String = prompt.chars().take(120).collect();
    format!(
        "<!DOCTYPE html><html lang='en'><head><meta charset='utf-8'><meta name='viewport' content='width=device-width,initial-scale=1'>\
<title>llmrace report</title><style>\
:root {{ color-scheme: dark; }}\
body {{ font-family: ui-monospace, 'SF Mono', Menlo, monospace; background: #0d1117; color: #e6edf3; margin: 0 auto; max-width: 780px; padding: 2rem 1rem; }}\
h1 {{ color: #3390ff; }} h2 {{ border-bottom: 1px solid #21262d; padding-bottom: .3rem; }}\
section {{ margin: 2rem 0; }} table {{ border-collapse: collapse; width: 100%; }} th, td {{ border: 1px solid #21262d; padding: .3rem .6rem; text-align: left; }}\
th {{ color: #8b949e; }} svg {{ width: 100%; height: auto; background: #161b22; border: 1px solid #21262d; border-radius: 6px; }}\
text {{ fill: #8b949e; font-size: 10px; }} .ttfb {{ fill: #8b949e; }} .gen {{ fill: #3390ff; }}\
.podium {{ list-style: none; padding: 0; }} .podium li {{ padding: .25rem 0; }} .medal {{ margin-right: .5rem; }} .score {{ color: #8b949e; }}\
.empty {{ color: #8b949e; }} footer {{ color: #484f58; margin-top: 3rem; }} code {{ color: #8b949e; }}
</style></head><body>\
<h1>🏁 llmrace report</h1>\
<p class='empty'>{} runs · max {} tokens · prompt: <code>{}{}</code> · {}</p>\
{body}\
<footer>generated with <a style='color:#3390ff' href='https://github.com/dfbustosus/llmrace'>llmrace</a> v{} 🦀</footer>\
</body></html>",
        runs,
        max_tokens,
        crate::util::html_escape(&label),
        if prompt.chars().count() > 120 { "…" } else { "" },
        crate::util::today_utc(),
        env!("CARGO_PKG_VERSION"),
    )
}

/// Horizontal per-run bars: grey TTFB segment + blue generation segment.
fn lane_bars(s: &LaneSummary) -> String {
    const WIDTH: f64 = 720.0;
    const ROW_H: f64 = 22.0;
    let n = s.runs.len().max(1);
    let max_total = s.runs.iter().map(|r| r.total_ms).fold(1.0, f64::max);
    let height = ROW_H * n as f64 + 26.0;
    let mut svg = format!(
        "<svg viewBox='0 0 {WIDTH} {height}'><text x='4' y='14'>{}</text>",
        crate::util::html_escape(&s.name)
    );
    for (i, r) in s.runs.iter().enumerate() {
        let y = 22.0 + ROW_H * i as f64;
        let ttfb_w = (r.ttfb_ms / max_total) * (WIDTH - 140.0);
        let gen_w = ((r.total_ms - r.ttfb_ms).max(0.0) / max_total) * (WIDTH - 140.0);
        svg.push_str(&format!(
            "<rect class='ttfb' x='60' y='{y}' width='{ttfb_w:.1}' height='12' rx='2'/>\
             <rect class='gen' x='{:.1}' y='{y}' width='{gen_w:.1}' height='12' rx='2'/>",
            60.0 + ttfb_w
        ));
        svg.push_str(&format!(
            "<text x='{WIDTH}' y='{:.1}' text-anchor='end'>{:.0} ms</text>",
            y + 10.0,
            r.total_ms
        ));
    }
    svg.push_str("</svg>");
    svg
}
