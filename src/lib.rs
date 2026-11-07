//! # llmrace
//!
//! Race LLM providers head-to-head in your terminal.
//!
//! The crate is organised as:
//! - [`provider`]: provider registry, streaming request plumbing, SSE parsing
//! - [`bench`]: benchmark engine and statistics
//! - [`cost`]: per-model pricing and cost estimation
//! - [`report`]: markdown / json / terminal reporting
//! - [`tui`]: the live race view

pub mod bench;
pub mod cost;
pub mod provider;
pub mod report;
pub mod tui;
