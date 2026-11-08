# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Shell completions: `llmrace --completions <bash|zsh|fish|powershell|elvish>`

### Changed

- `--duration N` now runs measured rounds until N seconds have actually elapsed (previously a round-count estimate)

### Fixed

- TUI: lanes in `--sweep` runs are now keyed by provider **and** variant; before, all variants of one provider streamed into a single lane and token counts collided

## [0.4.0] - 2026-11-08

### Added

- README: real-world benchmark section (local Ollama llama3.2:1b: 60.2 tok/s, sub-ms TTFB, 3/3 valid tool calls)
- Optional SQLite history backend: build with `--features sqlite` and run with `--storage sqlite` (transactional writes, `(provider, timestamp)` index); JSONL remains the default
- Static HTML dashboard: `llmrace --dashboard [--out file.html]` renders recorded history into a self-contained dark-theme page with per-provider SVG time series, median guides, model-swap annotations and last-run tables — no server, no JavaScript
- Prometheus pushgateway export: `--push-gateway URL` PUTs gauges (TTFB, latency mean/median/p95, tokens/s, cost, tool validity) in text exposition format after each race
- Tool-call benchmark mode: `--tools` races a weather-function task with per-wire tool schemas (OpenAI, Anthropic, Gemini, Cohere), decodes streamed argument fragments, and reports `tool_validity` (valid JSON containing `city`) per provider in JSON reports
- Provider: Cohere (native v2 chat streaming with legacy v1 decoding)
- Providers: xAI Grok, Perplexity Sonar
- Azure OpenAI support via `AuthKind::ApiKeyHeader` (`ProviderSpec::azure_openai`)
- Percentile histograms: `--histogram` renders p10–p99 ladders and block-bar latency distributions
- Doc-tests across cost, variants, regression and history modules

## [0.3.0] - 2026-11-07

### Added

- Persistent history and trends: `--record` appends every race to `~/.local/share/llmrace/history.jsonl`; `--trend [--days N] [providers…]` renders per-provider TTFB/throughput drift versus the rolling median with model-swap annotations
- Homebrew distribution: `brew install dfbustosus/tap/llmrace` ([tap](https://github.com/dfbustosus/homebrew-tap))
- Watch mode: `llmrace --watch --every 900` re-runs the race on an interval, compares TTFB and p95 against rolling medians, prints `⚠ ALERT` lines on degradation, and exits gracefully on Ctrl+C with a session summary and final report
- Prompt-variant sweeps: `--sweep short,long,code,json,reasoning` runs the full race once per prompt shape; reports emit one ranked table per variant and JSON entries carry a `variant` label
- `--profile <name>`: benchmark profiles loaded from `llmrace.toml` (repo root or XDG config), explicit CLI flags win (#20)
- Regression mode: `--baseline <report.json> --max-regression <pct>` compares fresh results against a saved report, prints an old → new diff table and exits `1` when any provider regresses (#16)
- Providers: Google Gemini native streaming (#17); DeepSeek, Mistral, Together, Fireworks, Cerebras (#18)
- Supply-chain CI: cargo-deny (advisories, licenses, sources) and dependabot for cargo + actions (#23, #24)

### Changed

- MSRV raised from 1.82 to **1.88**: the 2026 dependency graph (clap 4.6 / darling, ICU4X, ratatui) requires edition-2024-capable toolchains (#22)
- **Breaking (pre-1.0)**: `SseParser::feed` returns `Result` and enforces a 1 MiB buffer cap, erroring on separatorless floods (#48)

### Fixed

- Saturation-throughput aggregation under `--concurrency N`: rounds now aggregate wall-clock window instead of averaging per-stream rates

## [0.2.0] - 2026-11-07

### Added

- Live TUI race with per-lane streaming token counts, TTFB and finish times
- Benchmarks: TTFB, tokens/s over the generation window, p50/p95 with linear interpolation, 95% Student-t confidence intervals, 3σ outlier flagging, warmup rounds excluded from stats
- `--concurrency` saturation mode, `--timeout` mid-stream enforcement, panic containment per request
- Reports: markdown (paste-ready), JSON (full per-run data), CSV
- Custom OpenAI-compatible endpoints via `--endpoint` / `--provider-name` / `--api-key-env` (vLLM, LiteLLM, Together…)
- Library-grade core: typed `thiserror` errors, `#![deny(missing_docs)]`, `#![deny(clippy::unwrap_used)]`, zero-copy SSE parser with exhaustive chunk-boundary tests
- Criterion benchmarks for SSE parsing and statistics
- Cross-platform CI (Linux, macOS, Windows): fmt, clippy `-D warnings`, tests, benches, docs, end-to-end mock race
- Release pipeline: tagged builds ship binaries for 4 targets with SHA-256 checksums and auto-publish to crates.io

## [0.1.0] - 2026-11-07

### Added

- Initial release: race OpenAI, Anthropic, Groq, OpenRouter and local Ollama with a live TUI and markdown/JSON reports.

[Unreleased]: https://github.com/dfbustosus/llmrace/compare/v0.4.0...HEAD
[0.4.0]: https://github.com/dfbustosus/llmrace/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/dfbustosus/llmrace/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/dfbustosus/llmrace/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/dfbustosus/llmrace/releases/tag/v0.1.0
