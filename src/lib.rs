//! # llmrace
//!
//! Race LLM providers head-to-head: streaming latency, time-to-first-byte,
//! throughput and cost, measured from real streamed completions.
//!
//! The crate is organised as:
//! - [`mod@bench`]: the benchmark engine and lane summaries
//! - [`config`]: named profiles from `llmrace.toml`
//! - [`cost`]: per-model pricing and cost estimation
//! - [`dashboard`]: static HTML dashboard rendered from recorded history
//! - [`doctor`]: environment diagnostics (keys, endpoints, daemons, config, history)
//! - [`error`]: typed error surface of the `llmrace` library
//! - [`gates`]: TTFB quality gates over race results
//! - [`history`]: persistent run history and trend analysis
//! - [`merge`]: cross-region merging of distributed probe reports
//! - [`notify`]: alert webhooks and desktop notifications
//! - [`prom`]: Prometheus text-format rendering and pushgateway export
//! - [`provider`]: provider registry, wire protocols, streaming plumbing
//! - [`race`]: a high-level multi-provider race for library callers
//! - [`recommend`]: provider recommendations from accumulated race history
//! - [`regression`]: baseline comparison for CI quality gates
//! - [`report`]: markdown / JSON / CSV / HTML / JUnit exports
//! - [`sarif`]: SARIF 2.1.0 output for code-scanning integrations
//! - [`schema`]: JSON Schema for `race.json` reports
//! - [`score`]: composite winner scoring and the podium
//! - [`sqlite`]: optional SQLite history backend (feature `sqlite`)
//! - [`sse`]: incremental Server-Sent Events parsing
//! - [`stats`]: descriptive statistics with t-based confidence intervals
//! - [`suite`]: multi-race benchmark suite orchestration
//! - [`tools`]: tool-calling task registry, schemas, and argument validation
//! - [`tui`]: the live terminal race view
//! - [`util`]: shared helpers for escaping, redaction, truncation, dates, ranking
//! - [`variants`]: built-in prompt variants for multi-shape sweeps
//! - [`watch`]: continuous monitoring and rolling-baseline degradation alerts
//! - [`workload`]: a shared JSONL prompt file for a race, a suite and a ramp
//!
//! ## Example
//!
//! Run a quick deterministic race against the built-in mock provider:
//!
//! ```
//! use llmrace::bench::{spawn_lane, BenchConfig};
//! use llmrace::provider::ProviderSpec;
//!
//! # fn main() {
//! # let rt = tokio::runtime::Runtime::new().unwrap();
//! # rt.block_on(async {
//! let config = BenchConfig {
//!     prompt: "hello".into(),
//!     runs: 1,
//!     warmup: 0,
//!     ..BenchConfig::default()
//! };
//! let (handle, _progress) = spawn_lane(
//!     reqwest::Client::new(),
//!     ProviderSpec::mock(),
//!     "mock-1".into(),
//!     config,
//! );
//! let summary = handle.await.unwrap();
//! assert!(summary.successful());
//! # });
//! # }
//! ```

#![deny(missing_docs)]
#![deny(clippy::unwrap_used)]
// 100% safe Rust, enforced at compile time — this crate renders untrusted
// provider output and must never need unsafe to do it.
#![forbid(unsafe_code)]

pub mod bench;
pub mod config;
pub mod cost;
pub mod dashboard;
pub mod doctor;
pub mod error;
pub mod gates;
pub mod history;
pub mod merge;
pub mod notify;
pub mod prom;
pub mod provider;
pub mod race;
pub mod recommend;
pub mod regression;
pub mod report;
pub mod sarif;
pub mod schema;
pub mod score;
#[cfg(feature = "sqlite")]
pub mod sqlite;
pub mod sse;
pub mod stats;
pub mod suite;
pub mod tools;
pub mod tui;
pub mod util;
pub mod variants;
pub mod watch;
pub mod workload;
