# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Library module `llmrace::gates`: `--max-ttfb` gate logic (`ttfb_offenders`, `render_ttfb_verdict`) is now reusable and unit-tested from CI pipelines
- `Tui::render` is public — the race view can be drawn against any ratatui backend (used by the new TestBackend render tests)

### Changed

- `--doctor` probes keyed provider endpoints concurrently instead of sequentially — several blocked endpoints no longer stall diagnostics behind back-to-back 3s timeouts
- Profiles accept `retries` (`llmrace.toml`), included in the `--init` starter

### Fixed

- **Total-outage runs now exit non-zero**: when every lane fails, `--max-ttfb` and `--baseline` previously passed (failed lanes never gate) — a green CI pipeline over a complete outage. `llmrace` now prints `every provider failed` and exits 1
- `--baseline` with `--sweep`: variant lanes now compare against the provider's matching variant baseline entry (falling back to a plain entry) instead of all variants matching the first entry; skipped providers are reported per variant

### Security

- **HTML report and dashboard escape user input** (provider, model, prompt, error text): a model id like `<script>` previously landed raw in `--html` output — stored XSS in any shared report. Regression-tested with hostile inputs
- CSV report fields containing commas, quotes or newlines are now RFC-4180 quoted (custom provider/model names could previously corrupt row structure)

- **Reasoning-token visibility**: reports now split `visible_tokens` from hidden `reasoning_tokens` (provider usage minus the streamed estimate); reasoning lanes are annotated `🧠n` in terminal and markdown tables. Live-verified against gpt-5-nano (255 hidden reasoning tokens on a 256-token budget)
- **Cross-region merge**: `llmrace --merge a.json,b.json,…` renders a per-provider region table with ⚡ (best TTFB) and 🚀 (best throughput) markers, completing the distributed-probes story; `--region <tag>` stamps reports (also used by the probe workflow), and `--merge --json` writes a machine-readable merged report
- Providers: **LM Studio** (`lmstudio`, port 1234) and **llama.cpp llama-server** (`llamacpp`, port 8080) — opt-in locals excluded from auto-detect, with `--doctor` reachability checks
- `--watch --record`: watch sessions now append every round to the persistent history

### Changed

- SSE parser buffers are O(n) per block (rolling consumption window with compaction instead of an O(n) drain per event) — bench: −3% on the 200-event chunked feed, structurally linear for pathological chunking
- Percentile ladders sort once instead of per percentile — bench: 968µs → 138µs (7×) for the p10–p99 ladder on 10k samples
- `Sample::percentiles(&[f64])` added; single-percentile `percentile()` is unchanged

### Fixed

- `--json --out name.json` wrote `name.json.json` — the sibling-extension logic now replaces the extension instead of appending (also fixes double-suffixed probe artifacts)

## [1.0.0] - 2026-11-14

First stable release. The surface is frozen: CLI flags are a contract, library changes follow semver, report schemas only change additively, and `--baseline`/`--compare` keep parsing reports written since 0.3. No functional changes since 0.8.0 — this tag is the freeze.

## [0.8.0] - 2026-11-14

### Added

- `--html`: self-contained shareable race report — podium (with `--winner`), ranked table and per-run latency bars showing the TTFB vs generation split, no server or external assets

### Fixed

- **Honest throughput for reasoning models** (#125): tokens/s now divides usage tokens by the *effective* generation window — the observed streaming window widened to the provider-reported server processing time when longer. gpt-5/o-series runs previously reported burst artifacts (e.g. 30,000 tok/s); live-verified gpt-5-nano now reports ~200 tok/s, in line with gpt-4o-mini. TTFB was already honest (includes thinking time)
- Token-count fallback (when a provider reports no usage) estimates from delivered characters instead of counting SSE events, so coalesced multi-token deltas are no longer undercounted
- SSE parser accepts bare-CR line endings per spec (`\r\r` is a valid event separator)
- `--profile` now detects combined short flags (`-n5`, `-c8`) as explicit CLI overrides that win over profile values
- Docs: `--baseline` + `--sweep` comparison semantics documented

## [0.7.0] - 2026-11-14

### Added

- `provider=model` positional lanes: `llmrace openai=gpt-4o-mini openai=gpt-4.1` races several models of the same provider head-to-head in one run — TUI lanes, tables and the podium disambiguate them with `provider/model` labels

## [0.6.0] - 2026-11-14

### Added

- **Inter-token latency (ITL) metrics**: p50/p95 of the gaps between consecutive streamed tokens, plus a stutter count (gaps over 250 ms) — streaming *smoothness* alongside raw throughput. Shown in the terminal and markdown tables, and as `itl_p50_ms`/`itl_p95_ms`/`stutters` in JSON reports
- `--retries N`: retry a failed round up to N additional times before it counts as failed — resilience against transient provider errors in CI runs
- `--winner` (+ `--weights ttfb=30,tps=40,p95=30`): composite podium scoring across successful lanes, best-normalized per metric (winner scores 100); included in the markdown report as a Podium section
- `--compare a.json,b.json`: side-by-side diff of two saved JSON reports without racing — before/after deploy comparisons reusing the regression table

### Fixed

- Markdown report table was misaligned: the header lacked the Server and Network columns, so every value after Latency rendered one column left and cost dangled outside the table
- TUI: a transient round error permanently marked the lane red even when later rounds succeeded — errors now clear when the lane recovers

## [0.5.1] - 2026-11-14

### Changed

- Dependencies: toml 1.2.0 → 1.2.1, rusqlite 0.41 → 0.42 (`sqlite` feature), clap_mangen 0.2 → 0.3
- CI: actions/checkout v7, actions/upload-artifact v7, codecov-action v7, action-gh-release v3

## [0.5.0] - 2026-11-10

## [0.4.2] - 2026-11-09

## [0.4.1] - 2026-11-08

### Added

- TUI: live streaming token preview — lanes show the actual generated text (`▊ The lighthouse stands...`) while streaming, and the provider rate limit (`ratelimit: 29999 left`) when finished
- `--max-ttfb <ms>`: CI gate that exits 1 when any provider's mean TTFB exceeds the threshold — simpler than a full baseline for basic latency alerting
- `llmrace --init`: creates a starter `llmrace.toml` with annotated profiles (fast, overnight, saturation, ci)
- `--quiet`: suppresses banner, progress lines and result table — for scripts and CI pipelines
- TUI: finished lanes display provider-reported server time (`⚡327ms sv`)
- **Server vs network latency breakdown**: llmrace now captures provider-reported server-side processing time from response headers (OpenAI's `openai-processing-ms`), plus request IDs and rate-limit status. Reports show `Server` (provider-side) and `Network` (measured TTFB minus server time) columns, letting users distinguish "slow model" from "far away server"
- Per-provider model overrides: `-m ollama=llama3.2:1b -m gpt-4o-mini` — plain entries stay global, `provider=model` entries target one lane, enabling mixed cloud/local races
- Live-API smoke tests (opt-in): `cargo test --test live_openai -- --ignored` validates the full HTTP+SSE path and honest cloud TTFB against real OpenAI
- TUI: finished lanes show an estimated blended price per million tokens when the model has known pricing
- Man page: `llmrace --man` emits roff for packaging (`llmrace --man > llmrace.1`)
- Machine-readable outputs: `--list --json` (full registry with availability) and `--doctor --json` (structured check results)
- `--price provider/model=input:output` (repeatable): override stale per-model pricing from the CLI; takes precedence over the built-in table
- `llmrace --doctor`: environment diagnostics — API keys, endpoint egress (any HTTP status counts), local Ollama, config discovery and history validity, with a bug-report-ready summary
- `--price provider/model=input:output` (repeatable): override stale per-model pricing from the CLI; takes precedence over the built-in table
- Shell completions: `llmrace --completions <bash|zsh|fish|powershell|elvish>`

### Changed

- `--duration N` now runs measured rounds until N seconds have actually elapsed (previously a round-count estimate)

### Fixed

- Markdown report TTFB column rendered empty (format specifier truncated the formatted string to zero characters)
- Trend deltas now capped at ±9999% and display as integer percentages (previously raw `+86088.6%` from comparing across the TTFB measurement fix boundary)
- Trend analysis excludes records with zero throughput or zero TTFB (failed runs no longer pollute trend windows)
- Demo GIF: now a real mixed race — cloud OpenAI lane vs local Ollama vs mock with per-provider models
- **gpt-5 / o-series support**: these models reject `max_tokens`; llmrace now sends `max_completion_tokens` for them (verified live against gpt-5-nano — previously HTTP 400, now races)
- Pricing table: removed models deprecated by OpenAI (gpt-4.1-nano, o3-mini), verified against the live API
- **TTFB measurement**: the clock now starts immediately before the request is sent, including connection, TLS and queueing — previously it started when response headers arrived, which reported impossible sub-millisecond cloud TTFB
- Security: rusqlite 0.32 → 0.41 — bundled SQLite updated past RUSTSEC-2026-0459 / CVE-2026-119375 (caught by the supply-chain workflow)
- README real-world results refreshed with honest cloud-vs-local data (OpenAI gpt-4o-mini vs local Ollama), including tool-call validity rates
- `llmrace.toml` profiles reject unknown keys — typos like `run` instead of `runs` now fail with the expected key named instead of silently using defaults
- Ollama 404 model-not-found errors now append an actionable hint (`ollama pull <model>` or pass `-m <model>`)
- Demo GIF: lanes now race with an explicitly pulled model (`-m llama3.2:1b`) — the previous recording failed both Ollama lanes with red 404 model-not-found errors
- Demo GIF tightened: four-lane sweep race (Ollama vs mock, short/json variants) and doctor closing scene; smaller canvas, no dead air, no table wrapping
- Demo GIF re-recorded against the live TUI (real Ollama lane streaming against the instant mock lane, finish flags, post-race table, trend output)
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

[Unreleased]: https://github.com/dfbustosus/llmrace/compare/v0.5.0...HEAD
[0.5.0]: https://github.com/dfbustosus/llmrace/compare/v0.4.2...v0.5.0
[0.4.2]: https://github.com/dfbustosus/llmrace/compare/v0.4.1...v0.4.2
[0.4.1]: https://github.com/dfbustosus/llmrace/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/dfbustosus/llmrace/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/dfbustosus/llmrace/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/dfbustosus/llmrace/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/dfbustosus/llmrace/releases/tag/v0.1.0
