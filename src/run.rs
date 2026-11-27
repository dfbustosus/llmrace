//! Race execution: orchestration, reporting, gates, watch and ramp modes.

mod collect;
mod config;
mod race;
mod ramp;
pub(crate) mod reports;
mod watch;

pub(crate) use collect::SignalGuard;
pub(crate) use config::{resolve_model, RunConfig};
pub(crate) use race::{run_race, run_race_with_stop};
pub(crate) use ramp::{parse_ramp_levels, ramp_race};
pub(crate) use watch::watch_race;
