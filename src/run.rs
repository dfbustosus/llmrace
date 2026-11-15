//! Race execution: orchestration, reporting, gates, watch and ramp modes.

mod collect;
mod config;
mod race;
mod ramp;
mod reports;
mod watch;

pub(crate) use config::{resolve_model, RunConfig};
pub(crate) use race::run_race;
pub(crate) use ramp::{parse_ramp_levels, ramp_race};
pub(crate) use watch::watch_race;
