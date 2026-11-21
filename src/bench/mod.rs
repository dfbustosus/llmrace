//! Benchmark engine: runs rounds of concurrent streaming requests per
//! provider and aggregates latency, throughput and token statistics.

mod engine;
mod metrics;
mod summary;

pub use engine::{spawn_lane, spawn_lane_stoppable};
pub use metrics::{BenchConfig, LaneUpdate, RunMetrics, STUTTER_MS};
pub use summary::{FailureClass, LaneSummary, RoundFailure};
