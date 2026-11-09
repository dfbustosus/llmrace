//! Persistent run history and trend analysis.
//!
//! `--record` appends one JSON line per lane to `history.jsonl` under the
//! XDG data directory; `--trend` replays that history into per-provider
//! degradation views, including model-swap annotations.

use crate::stats::Sample;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// One persisted lane summary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HistoryRecord {
    /// Unix timestamp (seconds) of the race.
    pub timestamp_unix: u64,
    /// Provider id.
    pub provider: String,
    /// Model benchmarked.
    pub model: String,
    /// Prompt variant label, when a sweep was recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
    /// Concurrency level of the rounds.
    pub concurrency: usize,
    /// Mean total latency in milliseconds.
    pub mean_total_ms: f64,
    /// p95 total latency in milliseconds.
    pub p95_total_ms: f64,
    /// Mean time-to-first-byte in milliseconds.
    pub mean_ttfb_ms: f64,
    /// Aggregate throughput in tokens per second.
    pub mean_tokens_per_sec: f64,
}

/// Default history file location: `$XDG_DATA_HOME/llmrace/history.jsonl`,
/// falling back to `~/.local/share/llmrace/history.jsonl`.
pub fn history_path() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("share")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("llmrace").join("history.jsonl")
}

/// Appends records to `path` as JSON lines, creating parent directories.
///
/// ```
/// use llmrace::history::{append, load, HistoryRecord};
///
/// let dir = std::env::temp_dir().join("llmrace-doctest");
/// let path = dir.join("history.jsonl");
/// let record = HistoryRecord {
///     timestamp_unix: 1_700_000_000,
///     provider: "mock".into(),
///     model: "mock-1".into(),
///     variant: None,
///     concurrency: 1,
///     mean_total_ms: 290.0,
///     p95_total_ms: 295.0,
///     mean_ttfb_ms: 14.0,
///     mean_tokens_per_sec: 87.0,
/// };
/// append(&path, &[record.clone()]).unwrap();
/// assert_eq!(load(&path), vec![record]);
/// # let _ = std::fs::remove_file(&path);
/// ```
pub fn append(path: &std::path::Path, records: &[HistoryRecord]) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    for r in records {
        let mut line = serde_json::to_string(r).map_err(io_other)?;
        line.push('\n');
        file.write_all(line.as_bytes())?;
    }
    Ok(())
}

/// Loads records from `path`, skipping blank or corrupt lines.
pub fn load(path: &std::path::Path) -> Vec<HistoryRecord> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    raw.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

fn io_other(e: serde_json::Error) -> std::io::Error {
    std::io::Error::other(e.to_string())
}

/// Trend statistics for one provider over a time window.
#[derive(Debug, Clone, Default)]
pub struct ProviderTrend {
    /// Provider id.
    pub name: String,
    /// Most recent model used.
    pub current_model: String,
    /// All distinct models seen in the window, oldest first.
    pub models_seen: Vec<String>,
    /// Records in the window (chronological).
    pub records: Vec<HistoryRecord>,
    /// Median TTFB in the window, ms.
    pub ttfb_median: f64,
    /// Latest TTFB, ms.
    pub ttfb_latest: f64,
    /// Median tokens/s in the window.
    pub tps_median: f64,
    /// Latest tokens/s.
    pub tps_latest: f64,
    /// Median p95 latency in the window, ms.
    pub p95_median: f64,
}

impl ProviderTrend {
    /// TTFB change of the latest run versus the window median, in percent.
    pub fn ttfb_delta_pct(&self) -> f64 {
        pct_delta(self.ttfb_median, self.ttfb_latest)
    }

    /// Throughput change of the latest run versus the window median, in percent.
    pub fn tps_delta_pct(&self) -> f64 {
        pct_delta(self.tps_median, self.tps_latest)
    }

    /// `true` when the provider switched models within the window.
    pub fn model_swapped(&self) -> bool {
        self.models_seen.len() > 1
    }
}

fn pct_delta(base: f64, current: f64) -> f64 {
    if base.abs() < f64::EPSILON {
        return 0.0;
    }
    (current - base) / base * 100.0
}

/// Builds per-provider trends from history within the last `days` days.
///
/// `now_unix` is the reference "now" (seconds); providers listed in `filter`
/// are kept, an empty filter keeps everything. The final record per provider
/// defines "latest"; a provider must have at least two records in the window
/// to be included.
pub fn build_trends(
    records: &[HistoryRecord],
    days: u64,
    now_unix: u64,
    filter: &[String],
) -> Vec<ProviderTrend> {
    let cutoff = now_unix.saturating_sub(days * 86_400);
    let mut names: Vec<String> = Vec::new();
    for r in records.iter().filter(|r| r.timestamp_unix >= cutoff) {
        if !names.contains(&r.provider) {
            names.push(r.provider.clone());
        }
    }
    names.retain(|n| filter.is_empty() || filter.contains(n));

    let mut trends = Vec::new();
    for name in names {
        let window: Vec<HistoryRecord> = records
            .iter()
            .filter(|r| r.provider == name && r.timestamp_unix >= cutoff)
            .cloned()
            .collect();
        if window.len() < 2 {
            continue;
        }
        let mut models_seen: Vec<String> = Vec::new();
        for r in &window {
            if !models_seen.contains(&r.model) {
                models_seen.push(r.model.clone());
            }
        }
        let ttfb_median =
            Sample::from_values(window.iter().map(|r| r.mean_ttfb_ms).collect()).median();
        let tps_median =
            Sample::from_values(window.iter().map(|r| r.mean_tokens_per_sec).collect()).median();
        let p95_median =
            Sample::from_values(window.iter().map(|r| r.p95_total_ms).collect()).median();
        let last = window.last().cloned().unwrap_or_else(|| window[0].clone());
        trends.push(ProviderTrend {
            name,
            current_model: last.model,
            models_seen,
            records: window,
            ttfb_median,
            ttfb_latest: last.mean_ttfb_ms,
            tps_median,
            tps_latest: last.mean_tokens_per_sec,
            p95_median,
        });
    }
    trends
}

/// Renders trends as a terminal table.
pub fn render_trend_table(trends: &[ProviderTrend], days: u64) -> String {
    let mut out = format!(
        "📈 llmrace trend — last {days} days\n{:<14} {:<24} {:>12} {:>10} {:>10} {:>10} {}\n",
        "Provider", "Model", "TTFB med→now", "Δ%", "Tok/s", "Δp95%", "Notes"
    );
    out.push_str(&"-".repeat(out.len()));
    out.push('\n');
    if trends.is_empty() {
        out.push_str("no history yet — run `llmrace ... --record` to start recording\n");
        return out;
    }
    for t in trends {
        let notes = if t.model_swapped() {
            format!("model swap: {}", t.models_seen.join(" → "))
        } else {
            String::new()
        };
        let ttfb = format!(
            "{}→{}",
            crate::stats::fmt_ms(t.ttfb_median),
            crate::stats::fmt_ms(t.ttfb_latest)
        );
        out.push_str(&format!(
            "{:<14} {:<24} {:>9} {:>+9.1} {:>10.1} {:>+9.1} {}\n",
            t.name,
            truncate(&t.current_model, 24),
            ttfb,
            t.ttfb_delta_pct(),
            t.tps_latest,
            pct_delta(
                t.p95_median,
                t.records.last().map(|r| r.p95_total_ms).unwrap_or(0.0)
            ),
            notes
        ));
    }
    out
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}

/// Converts a lane summary into a history record.
pub fn record_from_summary(s: &crate::bench::LaneSummary, timestamp_unix: u64) -> HistoryRecord {
    HistoryRecord {
        timestamp_unix,
        provider: s.name.clone(),
        model: s.model.clone(),
        variant: s.variant.clone(),
        concurrency: s.concurrency,
        mean_total_ms: s.mean_total_ms(),
        p95_total_ms: s.p95_total_ms(),
        mean_ttfb_ms: s.mean_ttfb_ms(),
        mean_tokens_per_sec: s.mean_tokens_per_sec(),
    }
}
