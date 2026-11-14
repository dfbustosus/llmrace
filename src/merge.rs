//! Cross-region merging of distributed probe reports.
//!
//! [`crate::provider`] races one region at a time; the reusable probe
//! workflow uploads region-tagged JSON artifacts. This module merges them:
//! every provider is broken down per region so the fastest region for a
//! model is visible at a glance.

use crate::error::{Error, Result};

/// One provider lane as recorded in a region-tagged report.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct MergedLane {
    /// Region tag from the report (`--region`) or the file stem.
    pub region: String,
    /// Provider id.
    pub provider: String,
    /// Model benchmarked.
    pub model: String,
    /// Mean total latency in ms.
    pub mean_total_ms: f64,
    /// p95 total latency in ms.
    pub p95_total_ms: f64,
    /// Mean time-to-first-byte in ms.
    pub mean_ttfb_ms: f64,
    /// Aggregate throughput in tokens/s.
    pub mean_tokens_per_sec: f64,
}

/// Parses one saved JSON report into merged lanes, skipping failed lanes.
///
/// The region comes from the report's top-level `region` field (written by
/// `--region`) or falls back to the file stem.
pub fn parse_report(raw: &str, file_stem: &str) -> Result<Vec<MergedLane>> {
    let v: serde_json::Value =
        serde_json::from_str(raw).map_err(|e| Error::Config(format!("invalid report: {e}")))?;
    let region = v
        .get("region")
        .and_then(|r| r.as_str())
        .unwrap_or(file_stem)
        .to_string();
    let mut lanes = Vec::new();
    let empty = serde_json::Value::Array(Vec::new());
    for p in v
        .get("providers")
        .unwrap_or(&empty)
        .as_array()
        .unwrap_or(&Vec::new())
    {
        let successful = p
            .get("successful")
            .and_then(|s| s.as_bool())
            .unwrap_or(false);
        if !successful {
            continue;
        }
        let num = |key: &str| p.get(key).and_then(|x| x.as_f64()).unwrap_or(0.0);
        lanes.push(MergedLane {
            region: region.clone(),
            provider: p
                .get("provider")
                .and_then(|x| x.as_str())
                .unwrap_or("?")
                .to_string(),
            model: p
                .get("model")
                .and_then(|x| x.as_str())
                .unwrap_or("?")
                .to_string(),
            mean_total_ms: num("mean_total_ms"),
            p95_total_ms: num("p95_total_ms"),
            mean_ttfb_ms: num("mean_ttfb_ms"),
            mean_tokens_per_sec: num("mean_tokens_per_sec"),
        });
    }
    Ok(lanes)
}

/// Renders merged lanes as a per-provider cross-region table; the best
/// TTFB cell is marked ⚡ and the best throughput 🚀.
#[must_use]
pub fn render_table(lanes: &[MergedLane]) -> String {
    let mut out = format!("🌍 llmrace cross-region merge — {} lane(s)\n", lanes.len());
    if lanes.is_empty() {
        out.push_str("no successful lanes in the given reports\n");
        return out;
    }

    let mut keys: Vec<(String, String)> = Vec::new();
    for l in lanes {
        if !keys.contains(&(l.provider.clone(), l.model.clone())) {
            keys.push((l.provider.clone(), l.model.clone()));
        }
    }

    for (provider, model) in keys {
        let group: Vec<&MergedLane> = lanes
            .iter()
            .filter(|l| l.provider == provider && l.model == model)
            .collect();
        let best_ttfb = group
            .iter()
            .map(|l| l.mean_ttfb_ms)
            .fold(f64::MAX, f64::min);
        let best_tps = group
            .iter()
            .map(|l| l.mean_tokens_per_sec)
            .fold(f64::MIN, f64::max);
        out.push_str(&format!("\n{provider} ({model})\n"));
        out.push_str(&format!(
            "  {:<18} {:>10} {:>10} {:>10}\n",
            "Region", "TTFB", "p95", "Tok/s"
        ));
        for l in &group {
            let ttfb = if l.mean_ttfb_ms == best_ttfb && best_ttfb > 0.0 {
                format!("{:.0} ⚡", l.mean_ttfb_ms)
            } else {
                format!("{:.0}", l.mean_ttfb_ms)
            };
            let tps = if l.mean_tokens_per_sec == best_tps && best_tps > 0.0 {
                format!("{:.1} 🚀", l.mean_tokens_per_sec)
            } else {
                format!("{:.1}", l.mean_tokens_per_sec)
            };
            out.push_str(&format!(
                "  {:<18} {:>10} {:>9.0}ms {:>10}\n",
                l.region, ttfb, l.p95_total_ms, tps
            ));
        }
    }
    out
}

/// Loads and merges several report files.
///
/// ```
/// use llmrace::merge::parse_report;
///
/// let raw = r#"{"region":"eu","providers":[
///   {"provider":"groq","model":"m","successful":true,
///    "mean_total_ms":100.0,"p95_total_ms":120.0,
///    "mean_ttfb_ms":20.0,"mean_tokens_per_sec":80.0}]}"#;
/// let lanes = parse_report(raw, "fallback").unwrap();
/// assert_eq!(lanes[0].region, "eu");
/// ```
pub fn load_files(paths: &[std::path::PathBuf]) -> Result<Vec<MergedLane>> {
    let mut all = Vec::new();
    for path in paths {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| Error::Config(format!("reading {}: {e}", path.display())))?;
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("report");
        all.extend(parse_report(&raw, stem)?);
    }
    Ok(all)
}
