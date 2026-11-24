//! # llmrace
//!
//! Race LLM providers head-to-head: streaming latency, time-to-first-byte,
//! throughput and cost, measured from real streamed completions.
//!
//! The crate is organised as:
//! - [`provider`]: provider registry, wire protocols, streaming plumbing
//! - [`sse`]: incremental Server-Sent Events parsing
//! - [`sqlite`]: optional SQLite history backend (feature `sqlite`)
//! - [`suite`]: multi-race benchmark suite orchestration
//! - [`stats`]: descriptive statistics with t-based confidence intervals
//! - [`bench`]: the benchmark engine and lane summaries
//! - [`config`]: named profiles from `llmrace.toml`
//! - [`cost`]: per-model pricing and cost estimation
//! - [`report`]: markdown / JSON / CSV exports
//! - [`race`]: a high-level multi-provider race for library callers
//! - [`sarif`]: SARIF 2.1.0 output for code-scanning integrations
//! - [`schema`]: JSON Schema for `race.json` reports
//! - [`regression`]: baseline comparison for CI quality gates
//! - [`gates`]: TTFB quality gates over race results
//! - [`score`]: composite winner scoring and the podium
//! - [`merge`]: cross-region merging
//! - [`notify`]: alert webhooks and desktop notifications of distributed probe reports
//! - [`variants`]: built-in prompt variants for multi-shape sweeps
//! - [`workload`]: a shared JSONL prompt file for a race, a suite and a ramp
//! - [`tui`]: the live terminal race view
//! - [`util`]: shared helpers for truncation, dates, ranking
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
