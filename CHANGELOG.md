# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- **Reports state the rounds actually measured**: `--duration` previously wrote its internal 100,000-round sentinel into markdown/JSON metadata (and a graceful interrupt would overstate too); metadata now reflects reality
- **Tool-mode reasoning attribution fixed** (live-found): streamed tool-argument tokens never counted toward the visible estimate, so classic models showed phantom `🧠` annotations in `--tools` mode (gpt-4o-mini: 🧠45); argument fragments and their characters now count, and the 🧠 annotation only renders for known reasoning models — live-verified: gpt-4o-mini clean at 53 tok/s, gpt-5-nano honestly 🧠499 in the same race
- `#![forbid(unsafe_code)]` at crate level (library and binary): the zero-unsafe audit result is now a compile-time guarantee, not a snapshot; the binary's single guarded `expect` replaced with a pattern match — runtime paths are panic-free by construction
- **Crates.io tarball slimmed by 2.5 MB**: the demo GIF, social preview and `.github` machinery shipped inside every `cargo install llmrace` download since 0.5 — now excluded (95 → 79 files; the schema, example workflow and release guide still ship)
- **Distributed pipeline validated end-to-end with real region diversity**: the probe workflow dispatched on a GitHub runner auto-detected "California", stamped it via `--region`, and its artifact merged with a "SouthAmerica" local report — the full probe → artifact → `--merge` story is proven, not just tested
- Demo GIF re-recorded with the significance verdict in the closing table (a real p = 0.85 "could be noise" call — the honesty is the feature)
- README real-world results refreshed with significance-era data (the p=0.010 verdict leads the story); `--duration` memory characteristics documented (~2 KB/round/lane; prefer `--watch` for horizon monitoring)
- TUI lanes flag reasoning models (🧠) when finished, matching the tables; `--histogram` gains a TTFB distribution ladder alongside the latency ladder
- `--models` works with custom endpoints: `--models myvllm --endpoint … --provider-name myvllm` lists any OpenAI-compatible server's models (bare `--models` includes the custom endpoint too)
- `--ramp` handles SIGINT gracefully: the in-flight phase finishes, remaining levels are skipped, and the partial curve is still reported
- **Graceful interrupt for headless races**: SIGINT during `--no-tui` (or piped-output) races now finishes in-flight rounds and writes partial reports instead of losing everything — previously only `--watch` handled Ctrl-C; additive `spawn_lane_stoppable` API
- `list_models` derives its URL with RFC 3986 `Url::join` semantics instead of string surgery — custom endpoints with ports, query strings or unusual paths resolve correctly (live-verified against OpenAI and Ollama)
- New `decode_payload` fuzz target: the provider-JSON decoder (an untrusted parser boundary) is now fuzzed alongside the SSE parser in CI
- README documents the ITL local-scheduling caveat

- Significance verdicts label same-provider multi-model races as `openai/gpt-4o-mini vs openai/gpt-4.1` (previously an ambiguous `openai vs openai`), mirroring the podium


## [1.3.0] - 2026-11-15

The breadth-and-reliability release: two more providers, price-aware podiums, watch hardening, and CI ergonomics end to end.

### Added

- Providers: **DeepInfra** (`deepinfra`) and **Hyperbolic** (`hyperbolic`), OpenAI-compatible with default-model pricing locked by the coverage test
- **Cost as a podium weight**: `--weights tps=60,cost=40` ranks price-aware; the cost axis applies only when every scored lane has pricing data (otherwise the weight is renormalized away, never silently favoring unpriced lanes)
- `--out -` streams the markdown report to stdout (siblings keep conventional filenames) — pipe straight into `$GITHUB_STEP_SUMMARY`
- **TTFB p50/p95** in JSON reports (`ttfb_p50_ms`, `ttfb_p95_ms`): tail first-token latency, not just the mean
- `--alert-rounds N` (with `--watch`): alerts fire only after N consecutive degraded rounds — one-off network blips no longer page you (default 1 keeps the immediate behavior)
- Model-not-found HTTP errors append a `llmrace --models <provider>` hint pointing at live discovery
- `--trend` shows the median ITL p95 column for providers whose recorded history carries it
- **Non-terminal stdout runs headless automatically**: forgetting `--no-tui` in CI no longer garbles output — llmrace detects piped stdout, runs headless and prints a one-line note
- History records reasoning data (`reasoning_tokens`, `thinking_ms_estimate`); the dashboard renders a thinking-time series for reasoning models; sqlite migrates additively
- The `race.json` schema is committed at [`docs/schema.json`](docs/schema.json) with a CI drift check, so CI consumers can reference a stable URL
- End-to-end `--baseline --require-significance` gate test through the real binary (record → replay → pass with an explanatory verdict; platform jitter is absorbed by the significance requirement)
- Maintainer release guide (`docs/release.md`) and a `cargo publish` dry-run before the real publish

### Fixed

- **`--watch` resolved one model from the first provider and raced every lane with it** — `openai=gpt-4o-mini ollama=llama3.2:1b --watch` asked Ollama for `gpt-4o-mini` and 404'd. New additive `watch_with_models` API races each lane with its own resolved model (regression-tested end-to-end)

## [1.2.0] - 2026-11-15

The statistics release: race rankings carry error bars, CI gates distinguish real regressions from noise, and `--ramp` finds each provider's saturation point.

### Added

- **Statistical significance verdicts**: reports run Welch's t-test on per-run latencies between the top two lanes — `⚖ groq vs openai: Δ -172 ms, significant (p=0.012)` in the terminal and markdown, structured in JSON (`significance.p_value`) — so rankings based on noise are called out as such
- **Significance-aware CI gates**: `--baseline`/`--compare` compute per-metric Welch p-values from per-run data (rendered inline: `p=0.012` / `p=0.61 n.s.`); `--require-significance` fails the gate only on statistically significant breaches — flaky thresholds over noise no longer fail pipelines
- **`--ramp 1,2,4,8` saturation-curve mode**: races every provider at each concurrency level and reports throughput per level with each provider's peak marked (⬇ on >10% degradation past it). Markdown curve, structured JSON (`mode: "ramp"`) and `--html` with an SVG per-provider saturation chart; conflicts with `-c`/`--sweep`/`--watch`
- **Custom prompt variants**: `[variants] sql = "…"` in `llmrace.toml` defines sweep shapes of your own; `--sweep sql,short` mixes them with built-ins (customs override built-ins by name)
- `--prompt-file <path>` (or `-` for stdin) for long prompts
- `docs/example-latency-gate.yml`: ready-to-copy GitHub Actions workflow for the significance-aware CI gate

### Changed

- Lane orchestration extracted into a shared `race_collect` core used by both the standard race and ramp phases

## [1.1.1] - 2026-11-15

### Fixed

- **Markdown reasoning lanes rendered `*****` instead of tokens/s**: the 🧠-annotated value was passed pre-formatted into the template's `{:.1}` slot, where precision truncated the string to one character; the value and marker now render (`**131.0** 🧠510`), regression-tested on the markdown path

### Added

- `llmrace --models [provider]`: lists the models a provider currently offers (OpenAI-compat `/v1/models`, Anthropic, Gemini and Cohere shapes); bare `--models` queries every auto-detected provider — no more guessing `-m` values
- `llmrace --schema`: prints the JSON Schema (draft 2020-12) for `race.json`, so CI consumers can validate reports with standard tooling; a test keeps the schema and real reports in sync
- `--tools` now conflicts with `--sweep` (both control the prompt; the tool prompt was silently discarded before)
- TUI lanes past 800 ms without a first token show `⏳ thinking… 1.9s` instead of the misleading `▶ streaming…` (reasoning models think before the first visible token)
- `--doctor` reports available llmrace upgrades from crates.io (one user-initiated GET; the race path never phones home)

## [1.1.0] - 2026-11-15

The metrics-honesty and hardening release: reasoning models measured truthfully end-to-end, distributed probes completed with cross-region merging, three security fixes, and six correctness fixes — including two where the tool silently reported wrong data (a pricing row that never matched, Prometheus series that overwrote each other).

### Added

- **Reasoning metrics end-to-end**: reports split `visible_tokens` from hidden `reasoning_tokens` (🧠n annotations in tables), tokens/s uses the effective generation window so burst delivery cannot inflate throughput (live-verified: gpt-5-nano 19,000 → 200 tok/s), and `thinking_ms_estimate` scales server time by the reasoning-token share
- **Cross-region merge**: `llmrace --merge a.json,b.json,…` renders a per-provider region table with ⚡ (best TTFB) and 🚀 (best throughput); `--region` stamps reports (probe workflow updated), `--merge --json` writes machine-readable output
- **Tool-task registry**: `--tools [weather|math|lookup]` — per-wire schemas and per-task argument validators; tool validity is scored against the lane's own task
- Providers: **LM Studio** (`lmstudio`, :1234) and **llama.cpp llama-server** (`llamacpp`, :8080) — opt-in locals excluded from auto-detect, with `--doctor` checks
- `provider=model` positional lanes race several models of one provider head-to-head (podium and tables disambiguate with `provider/model` labels)
- Inter-token latency (ITL p50/p95, stutter count >250ms) in tables and JSON; history records `itl_p95_ms`; dashboard renders an ITL series; sqlite migrates additively
- `--retries N` with linear backoff; `--winner` + `--weights`; `--compare a.json,b.json`; `--html` shareable report; `--region`
- `--watch --record`: watch sessions feed the trend history
- Library: `llmrace::gates` (TTFB gate, outage detection), `Sample::percentiles`, `provider::is_reasoning_model`, public `Tui::render`
- Racing a reasoning model with `--max-tokens` under 512 prints a starvation hint

### Changed

- **Honest throughput for reasoning models** (#125): tokens/s divides usage tokens by the effective generation window (observed window widened to provider-reported server time when longer); no-usage token fallback estimates from delivered characters instead of SSE event count
- Profile precedence uses clap `ValueSource` (no argv scanning; `-n 1`, `--runs 1`, `--runs=1` all reliably win); profiles accept `retries`
- `--doctor`: concurrent endpoint probes, spec-derived local-daemon checks, opt-in locals no longer report as failed when not running; `--list --json` includes `opt_in`
- SSE parser O(n) per block (bench-proven); percentile ladders single-sort (7× on 10k samples)
- `provider` split into `spec`/`request`/`stream`, `bench` into `metrics`/`summary`/`engine` — public API unchanged
- `--record` skips failed lanes (matching `--watch`); markdown notes lanes without pricing data
- Mid-stream timeouts name model, window and tokens received; README documents same-provider multi-model rate-limit sharing

### Fixed

- **Pricing lookups were case-sensitive**: the Together row never matched its lowercased key — its cost column has been empty since the table was added; a coverage test now locks every cloud default model to a pricing row
- **Total-outage runs exit non-zero**: `--max-ttfb`/`--baseline` previously passed when every provider failed (a green pipeline over a complete outage)
- Prometheus export: sweep variants carry a `variant` label (series no longer overwrite each other); provider names are label-escaped
- `--baseline` + `--sweep`: variant lanes match their variant baseline entry (fallback to plain) instead of all matching the first entry
- `--sweep --winner`: per-variant podiums — variants race different prompts and must not compete head-to-head
- TUI header showed `100000 rounds` with `--duration`; CSV injection (RFC-4180 quoting); `--json --out x.json` no longer writes `x.json.json`
- SSE parser accepts bare-CR separators per spec

### Security

- **Stored XSS in `--html` reports and the dashboard**: provider/model/prompt/error text landed raw in HTML; a model id like `<script>` executed in any shared report. All interpolations escaped, regression-tested with hostile inputs
- API keys never appear in any output — enforced by a leak-guard test over stdout/stderr/md/json/csv/html
- CSV fields with commas/quotes/newlines are RFC-4180 quoted

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
