# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [1.39.0] - 2026-11-26

Measurement integrity, consistent gates, and reproducible execution records.

### Added

- Per-request attempt records on round metrics and failures, request-level failure counts, and end-to-end output throughput. New public `RequestOutcome` / `RequestAttempt` types; `RunMetrics.requests`, `RoundFailure.requests` / `task_checked`, and `PromptRow.failed_rounds` are additive library fields in this minor release
- `experiment_sha256` and `measurement_version` identify execution settings as well as request content; model-aware baseline matching and explicit availability/configuration gate failures
- `regression.status` and missing-evidence details; `--fail-on-inconclusive`, MCP's matching option, and `gate_failed_with_policy` allow strict gates without calling untestable changes noise
- Consumer contract checks for real JSON Schema validation, CLI/JSON/JUnit/SARIF agreement, single-worker TUI execution, watch persistence, and offline wheel installation

### Fixed

- Concurrent failures are retained and only failed requests retry; round timings include failed attempts and backoff
- Usage-only events no longer start TTFT; malformed, errored and unterminated provider streams fail instead of accepting partial text as completion
- AWS event-stream CRC-32, header layout and value lengths now match the protocol, with an independently encoded fixture; invalid frames and exception events propagate immediately
- Fully failed and missing baseline lanes/prompts fail in the CLI, library and MCP; failed-only workload prompts remain in JSON and JUnit. Baseline structs gain optional execution/outcome fields, including `BaselinePrompt.rounds`
- Verdicts distinguish goodput leadership from mean latency and correctly describe slower leaders; Student-t confidence intervals use finite-sample critical values beyond 30 degrees of freedom
- Concurrent workload citations count rounds correctly; schema accepts unavailable confidence intervals and failed-only prompt rows; reports retain failure prompt IDs and raw attempt data
- TUI progress forwarding no longer blocks Tokio workers; unused library progress queues are closed
- Unbounded watch has bounded retention, alerts on outages, persists rounds immediately with their own timestamps through the chosen backend, and interrupts long sleeps promptly; Prometheus exports availability for failed lanes
- Recommendations use matching model/workload/variant/concurrency history, exclude currently unavailable providers, and price 30 days at 1,000 requests/day correctly
- Docker builds and manifest publication share one resolved release version; old-version backfills do not update rolling aliases. Release builds use the lockfile and verify tag/version identity
- Container and wheel packaging verify archive checksums; wheels include both license texts. CI installs the generated wheel and denies rustdoc warnings
- MSRV checks cover default features on Rust 1.88 and SQLite on Rust 1.89, documenting the existing `rusqlite 0.42` compiler requirement


## [1.38.0] - 2026-11-25

The first question every user asks — who won, by how much, and should I trust it — is now one sentence.

### Added

- A **Verdict line** after the terminal table and in the markdown report: the leader, its throughput and TTFT, prompts passed, how many times faster than the runner-up (or "within 10%"), and whether the gap is significant or could be noise — with "more rounds would tell" when it could not be called
- `race.json` carries the same sentence as an optional `verdict` string, so agents and pipelines read the conclusion without parsing the table

## [1.37.2] - 2026-11-25

### Fixed

- The PyPI project page now carries a real description: quick start, the race/gate/CI/MCP capability story, keywords, classifiers and issue links, instead of one bare paragraph

### Added

- `docs/demo-mcp.gif` in the README's MCP section and `examples/mcp-demo.sh` it records: `tools/list`, then a mock race returned as a tool result, runnable offline

## [1.37.1] - 2026-11-25

### Fixed

- `--suite` ignored `--weights` (it hardcoded the default podium weights), and its hand-rolled config could drift from the race path again. Suites now clone the one `RunConfig` the race builder produces and override only what an entry defines, so every flag reaches suites automatically

## [1.37.0] - 2026-11-25

An audit pass: three duplicated code paths are now one each, and the two unifications are public helpers.

### Added

- `ProviderSpec::parse("id" | "id=model")` — the one provider-argument parser the CLI, `--suite` and the MCP server all delegate to
- `report::insert_note` — the one way metadata notes (sampling, cache mix) reach a markdown report

### Changed

- The terminal table builds its header and rows from one shape, splicing the Pass column, instead of two near-identical format branches

## [1.36.0] - 2026-11-25

Agents can race providers as tools now, and any repo can gate prompts on commit.

### Added

- `llmrace --mcp`: an MCP server on stdio (newline-delimited JSON-RPC). Four tools — `llmrace_race` (markdown report), `llmrace_gate` (verdict JSON plus the regression section; `isError` when the gate fails), `llmrace_doctor` (configured providers, never key values), `llmrace_list` (the registry). Keys come from the environment; `mock` races offline
- `.pre-commit-hooks.yaml`: `llmrace-gate` runs the installed binary against your committed baseline in any repository using pre-commit

## [1.35.1] - 2026-11-24

### Fixed

- `--suite` now propagates `--warmup-mode` and `--retries` into every entry instead of silently resetting them to defaults; the CLI has one shared warmup-mode mapping
- The live OpenAI lane test asks for a paragraph, not a single word — a one-token answer can honestly measure under 10 tok/s, which is a test bug, not a measurement bug

### Added

- `tests/live_openrouter.rs`: env-gated live smoke tests for the OpenRouter wire (stream, honest TTFB, priced cost), mirroring `tests/live_openai.rs`. Both suites pass against the real APIs today

## [1.35.0] - 2026-11-24

Embedders can now watch a race live and stop it cleanly, through the public API.

### Added

- `Race::spawn()` returns a `RaceHandle`: `progress()` is a merged live stream of every lane's updates on a plain `std::sync::mpsc` channel (no async needed on the reading thread), `stop()` ends every lane after its in-flight round, and `finish()` awaits the usual `RaceOutcome`. `Race::run()` is now `spawn().finish()` — same behavior, one code path

## [1.34.0] - 2026-11-24

Structured output is now raceable: force JSON mode and measure what it costs, then grade whether the answer complied.

### Added

- `--json-output` sets `response_format: json_object` (OpenAI chat, Cohere), `text.format` (Responses API) or `responseMimeType: application/json` (Gemini, Vertex)
- `--response-schema FILE` sends a JSON Schema instead: `json_schema` with `strict: true` on OpenAI wires, `responseSchema` on Gemini wires. An invalid or non-object schema fails before any request
- `Sampling::json_output` (`llmrace::provider::JsonOutput`) for library callers; the request hash changes only when enforcement is set, and `race.json` stamps it under `sampling.json_output`
- Anthropic, Bedrock and the mock have no such field and ignore the control — documented so races stay comparable

## [1.33.0] - 2026-11-24

Races through OpenRouter price every lane now.

### Added

- OpenRouter price rows, read from the live models API on 2026-11-24: `anthropic/claude-haiku-4.5` ($1.00/$5.00, cache read $0.10), `anthropic/claude-haiku-5.5`, `deepseek/deepseek-chat-v3.1`, `deepseek/deepseek-chat`, and `meta-llama/llama-3.3-70b-instruct`. The report footnote names the API and the date
- A regression test locks the rows behind the four-model demo race so the cost column cannot silently empty again

## [1.32.1] - 2026-11-24

### Fixed

- The markdown per-prompt table names the model (`openrouter/anthropic/claude-haiku-4.5`) when several lanes share a provider, instead of repeating the bare provider name — the same rule the significance line already used

## [1.32.0] - 2026-11-24

The terminal table shows what the workload proved, and the demo is a real model giving a real wrong answer.

### Added

- The terminal table gains a **Pass** column whenever the race had a task (`--workload` with `expect`, or `--tools`). A race with no task keeps today's shape
- `examples/workloads/demo-local.jsonl`: three short prompts with `contains`, `exact` and `contains` matching, sized so a small local model passes two and honestly fails one

### Changed

- The demo GIF is a real Ollama race (llama3.2:1b, no API key) on that workload: per-lane Pass counts, the significance line, and the podium

## [1.31.0] - 2026-11-24

Gate failures can now land in the Security tab and on the PR checks pane, with no bot.

### Added

- `--sarif` writes `race.sarif` (SARIF 2.1.0): every condition that fails a gate — a regression beyond the threshold, a changed request, a TTFB breach, a lane that never succeeded — is one `result` with the provider, the metric, and the prompt id. Upload with `github/codeql-action/upload-sarif`
- `llmrace::sarif::sarif` for library callers, and `MetricComparison::fails_the_gate` is now public so embedders apply the same rule the exit code uses
- `--out race.sarif` implies `--sarif`, matching the other report extensions

## [1.30.0] - 2026-11-24

The race now speaks the one format every CI system already understands.

### Added

- `--junit` writes `race.xml`: one `<testsuite>` per lane and one `<testcase>` per checked prompt (per lane when the race had no task). A prompt whose task did not pass every measured round, or that had HTTP failures, is a failing test — so GitHub, GitLab and Buildkite test reporters show it on the pull request with no bot
- `RaceOutcome::junit()` in the library
- `--out race.xml` implies `--junit` and writes markdown to `race.md`, matching the other report extensions

## [1.29.0] - 2026-11-23

Warmup can no longer secretly prime the cache, a mixed cache sample says so, and OpenAI's Responses API is a first-class wire.

### Added

- `--warmup-mode probe`: warmup sends a minimal `ping` (`max_tokens: 1`, no system prompt) so the connection warms without priming the provider's prefix cache for a workload prompt. The default `same-prompt` behavior is unchanged
- `cache_hit_rounds` per lane in `race.json`, and a `Cache mix` note in the markdown report when a lane mixes cold and warm rounds — the latency sample is then not homogeneous
- `Wire::OpenAiResponses`: the OpenAI `/v1/responses` API, decoding `response.output_text.delta`, streamed tool arguments, and `response.completed` usage (cached input, reasoning tokens). Available as the opt-in `openai-responses` provider and as `--wire responses` on a custom endpoint

## [1.28.0] - 2026-11-23

The crate is now a library a Rust test suite can drive: one builder runs the whole race, and price overrides no longer live in a process-global map.

### Added

- `llmrace::race`: `Race::new(providers)` builder (`prompt`, `workload`, `runs`, `warmup`, `concurrency`, `max_tokens`, `timeout`, `retries`, `sampling`, `reproducible`, `tools`, `models`, `price_book`, `region`, `winner`) returning a `RaceOutcome` with `summaries`, `request_sha256`, `podium()`, `markdown()`, `json()` and `gate(baseline, GatePolicy)`
- `llmrace::cost::PriceBook`: scoped per-race rate overrides. A lane stamped with a book prices itself from the book plus the built-in table; the process-global override map is not consulted, so parallel races and tests cannot leak prices into each other
- `examples/cargo_test_gate.rs`: a workload race gated against its own baseline, the shape of a `#[tokio::test]`

## [1.27.0] - 2026-11-23

A workload prompt can be a conversation, a JSON document, or a tool call. A line that uses none of those stays the request it was.

### Added

- A workload line may set `messages` (a conversation ending on a user turn) and `system` (this round only). OpenAI-compatible, Anthropic, Cohere, Gemini, Vertex, and Bedrock each send that conversation in their own message shape
- `match: json_schema` checks the trimmed answer against the schema in `expect`. The schema is rejected before any request when it uses a keyword this crate does not evaluate
- A workload line may set `tool` with a name, description, and JSON Schema. The streamed arguments are checked against that schema on every wire, including Bedrock and Vertex
- `request_sha256` records `system`, `messages`, and `tool` only when the line sets them, so an exact short-answer workload keeps its hash

## [1.26.0] - 2026-11-23

The gate can fail on one prompt's latency, and the markdown report carries the same verdict as `race.json`.

### Added

- `providers[].runs[]` includes `prompt_id` when the round came from a workload
- A baseline that recorded a prompt's `mean_total_ms` or `mean_ttfb_ms` fails the gate when that prompt's latency or time to first token rises past the threshold. The comparison uses Welch's test on the rounds of that prompt when both sides have at least two. A baseline that omitted the fields skips them
- The markdown report includes a `## Regression` section when `--baseline` is set, with the same pass or fail as `regression.ok`

## [1.25.1] - 2026-11-23

Fixes output footguns on piped and extension-typed report paths, HTML `--duration` metadata, and stale architecture/example files.

### Fixed

- `--out -` now routes banners, terminal tables, and file-save notices to `stderr` so `stdout` receives pure Markdown even without `--quiet`
- `--out foo.json` (and `.csv` / `.html`) writes that format directly to the requested path and writes Markdown to `foo.md`, instead of writing Markdown into a `.json` file or overwriting it twice when `--json` is also set
- `--html` now records the rounds actually measured (preventing the `100000` sentinel on `--duration` runs) and applies automatic task/cost podium weights identically to terminal and Markdown reports
- The `--suite` report footer now states that tables are ranked by goodput (`tokens/s × success rate`), matching `report::variant_table`

### Changed

- The README no longer shows a drawn terminal. The demo GIF is the picture of the race
- Updated `examples/ci-gate.yml`, `ARCHITECTURE.md`, `RELEASE.md`, and `llmrace.example.toml` to match current behavior and workflows

## [1.25.0] - 2026-11-22

The front of the repo shows the race a stranger can run, and what the crate decides.

### Changed

- The README opens on the race, the per-prompt answer, the gate that names the prompt, and the noise check. Quick start begins with `llmrace mock mock --winner`, which needs no API key
- The demo records that no-key race: the table, the significance line, and the podium. It no longer needs `OPENAI_API_KEY`
- The crates.io description and keywords say latency and the gate, which is what people search for

## [1.24.0] - 2026-11-21

The JSON a pipeline uploads now carries the gate verdict. The README's first CI commands are the workload race.

### Added

- When `--baseline` is set, `race.json` writes `regression`: `ok`, `request_mismatch`, `threshold_pct`, and `failures` with `provider`, `metric`, `prompt_id`, `baseline`, `current`, and `delta_pct`. `ok` is false exactly when the process exits 1 for the gate. A request mismatch leaves `failures` empty. The field is optional, so an older report still validates
- The terminal table stays for a person. A pipeline reads `regression`

### Changed

- The CI section records a workload baseline and compares the same workload. A race with no `expect` still has no task-success metric

## [1.23.0] - 2026-11-21

`--reproducible` pins sampling only when asked. A race that omits it still uses each provider's own default.

### Added

- `--reproducible` fills whichever of temperature, top-p and seed are still unset, after the profile is applied: `0`, `1` and `0`. A flag or a profile value already set wins
- Those three values are already part of the request hash, so a pinned race and an unpinned race hash differently and the gate refuses the comparison

OpenAI reasoning models still ignore temperature, top-p and seed on the wire.

## [1.22.0] - 2026-11-21

`expect` can be a substring or a regex. A line that only has `expect` stays an exact check, and its request hash does not change.

### Added

- A workload line may set `match` to `exact` (the default), `contains`, or `regex`. An unknown value is rejected when the file is read
- `contains` is a case-sensitive substring of the trimmed answer. `regex` is compiled then, with a 1 MiB size limit, against the trimmed answer. An invalid pattern fails before any request
- The request hash records `match` only when it is not `exact`, so a 1.19 baseline of an unchanged exact file still compares

## [1.21.0] - 2026-11-21

A lane score of 2/3 no longer hides which prompt missed. The per-prompt table names the pass count, and the gate fails when one prompt's success falls.

### Added

- The markdown per-prompt table has a Pass column (`ok/n`, or `—` when that prompt had no task). JSON `prompts` items carry `task_pass` as `[ok, n]` or null, and `task_pass_rate`. Both fields are optional, so an older report still validates
- A failed round records its prompt id. That round counts in the prompt's denominator, so a dropped round is not a pass
- The regression gate compares `prompts[].task_pass_rate` for every id present on both sides, beside the lane's task success, using the same threshold. A missing p-value counts. A prompt the baseline omitted is skipped. A baseline with no per-prompt rates still gates the lane only

## [1.20.0] - 2026-11-21

The repository's own CI now runs the task-success gate, and a tag whose crate is already on crates.io no longer fails the publish job.

### Added

- CI races the mock provider. A drop in task success fails the job. The same request passes

### Changed

- The crates.io publish job skips a version that is already published, instead of failing the tag

## [1.19.0] - 2026-11-21

A saved race and a new race are compared only when they made the same request. `prompt_sha256` is still the prompt texts. `request_sha256` also covers `expect`, `max_tokens`, sampling, the system prompt, and the tool task.

### Added

- `race.json` writes `request_sha256`. History and the optional SQLite store keep it. Older files still load
- The regression gate fails, and does not compare metrics, when both sides recorded the hash and they differ. A baseline that omitted it still compares
- `--trend` averages only the rows that share the latest request, and notes when the window also held another one

## [1.18.0] - 2026-11-21

`llmrace gemini` and `llmrace vertex` call `gemini-3.5-flash-lite`. On 2026-12-01 Google routes `gemini-2.5-flash` to that id and bills it at the Flash-Lite rate. `gpt-4o-mini` and `claude-3-5-haiku-latest` stay the OpenAI and Anthropic defaults.

### Changed

- Gemini and Vertex default model id is `gemini-3.5-flash-lite`. The paid text rate, read 2026-11-21, is $0.30 in, $2.50 out, cache read $0.03. `gemini-2.5-flash` stays in the price table
- Vertex stays experimental. The live path was not run

## [1.17.0] - 2026-11-21

`race.json` names the price date, the workload, and the prompts that were sent. The 2026-11-15 results table was not re-run.

### Added

- `pricing_as_of` is the price-table date (`2026-11-20`)
- `workload` is the prompt ids in measured-round order. It is empty when the race was one string
- `prompt_sha256` is the hex SHA-256 of the prompt texts those rounds sent, joined with a newline. It identifies the workload; it is not a signature

## [1.16.0] - 2026-11-21

`llmrace gemini` and `llmrace vertex` call `gemini-2.5-flash`. Google shut `gemini-2.0-flash` down on 2026-06-01. `gpt-4o-mini` and `claude-3-5-haiku-latest` stay the OpenAI and Anthropic defaults: both still serve, and changing them would change every existing race's latency and bill.

### Changed

- Gemini and Vertex default model id is `gemini-2.5-flash`. The old id stays in the price table so a saved race still prices
- Vertex stays experimental. The live path was not run; no Google credentials were available

## [1.15.0] - 2026-11-21

A wrong answer now fails the CI gate for the same reason it loses the report. A race with no task still has no task metric, so a plain latency gate does not move.

### Added

- History and the optional SQLite store keep `task_pass_rate` (ok/measured). Older JSONL and older databases still load
- The regression gate fails when task success falls, in percentage points, beside failure rate. A baseline that omitted the field skips it. A missing p-value counts, because it is a ratio of the whole sample
- `--recommend` says when a provider led on goodput and lost on task success
- `race.json` writes `task_pass_rate` next to `task_pass`

### Changed

- The example CI workflow races `examples/workloads/short-answer.jsonl` with `--json` and `--require-significance`, and uploads the JSON that command writes

## [1.14.0] - 2026-11-20

The price table now says when it was read and which page it came from. Bedrock and Vertex stay experimental: no live account was available to run them.

### Added

- OpenAI, Anthropic and Gemini rates are stamped 2026-11-20, from the providers' pricing pages. The report prints that stamp under the cost column
- Rows for the models people race now, without a row for every new id: `gpt-5.4`, `gpt-5.4-mini`, `gpt-5.4-nano` (short context), `claude-sonnet-4-6`, `claude-haiku-4-5`, `gemini-2.5-flash`
- `cost::PRICING_AS_OF`, `cost::pricing_sources` and `cost::pricing_stamp`

## [1.13.0] - 2026-11-20

A fast wrong answer can lose. Task success is a successful request that failed the check, not a failed HTTP call, and a race with no task keeps the previous podium.

### Added

- Pass column (`ok/measured`) in the markdown and HTML tables. JSON adds `task_pass`. `—` when the race had no task
- `--tools` checks tool-argument validity. A workload `expect` checks an exact short answer: the trimmed text, a JSON object (key order does not matter), or a number
- `--winner` weights gain `task` (default 40). It is ignored when no scored lane had a task, the same way `cost` is ignored when a lane has no price. A plain latency race scores as before
- `tools::answer_matches`, `tools::task_ok`, `LaneSummary::task_pass` and `LaneSummary::task_rate`
- `RunMetrics::task_ok`

## [1.12.0] - 2026-11-20

One prompt is a demo. A race, a suite and a ramp can now share a workload, and a slow prompt shows up on its own row.

### Added

- `--workload FILE` reads a JSONL file, one `{"id","prompt","expect"?}` per line. Every lane draws the same prompt on a given round, in order, and wraps. `--ramp` uses that file. A suite entry sets `workload` to the same path. `--prompt-file` is still one prompt
- The markdown report gains a Per prompt table. JSON gains a `prompts` array on each lane (`id`, rounds, mean latency, TTFT, tokens/s) when a workload was used
- `workload::parse`, `workload::load` and `BenchConfig::workload`
- Built-in files `examples/workloads/short-answer.jsonl` and `examples/workloads/tool-call.jsonl`. `expect` is stored and not scored yet

## [1.11.0] - 2026-11-20

The number on the screen and the number in CI are the same number. Failure rate, goodput, retries and cost now survive `--record`, `--baseline` and `--recommend`, and a reasoning model's bill includes the tokens the provider actually charged.

### Fixed

- **The production loop keeps failed rounds.** History records `failure_rate`, `goodput_tokens_per_sec`, `retry_attempts` and `cost_per_run_usd`. Older JSONL still loads. SQLite adds the same columns
- `--baseline` fails when the failure rate rises (percentage points), when goodput falls, or when a priced cost/run rises, beside the latency and tokens/s checks. A baseline that never recorded the field skips it. `--require-significance` still filters the noisy means; failure rate, goodput and cost are whole-sample ratios, so a threshold breach on those counts
- `--recommend` ranks throughput on goodput and says when a provider's failure rate is why it lost the lead. A recorded cost is used instead of re-estimating one
- **Gemini thoughts are billed as output.** `thoughtsTokenCount` is not inside `candidatesTokenCount`, so it is added to the output bill. OpenAI `completion_tokens` already includes `reasoning_tokens`, which is recorded and not added twice. The visible-stream estimate remains only when the provider sent no count
- `--ramp` marks the concurrency where goodput peaks, and JSON names the first level whose failure rate crosses 5%

### Added

- Reports print TTFT for the existing TTFB clock. JSON adds `ttft_ms`, `tpot_p50_ms` and `tpot_p95_ms` (time per output token: the generation window divided by completion tokens)
- `HistoryRecord` and `BaselineProvider` carry the new fields, all serde-default
- `ProviderProfile::median_goodput`, `median_failure_rate` and `rank_throughput`
- `StreamEvent` and `RunMetrics` carry `reasoning_tokens` and `reasoning_billed_separately`
- `LaneSummary::tpot_ms`

## [1.10.0] - 2026-11-20

A failed round used to disappear. Latency, tokens/s and the winner were computed from whatever survived, so a provider that timed out half its calls could beat one that answered every call.

### Fixed

- **Failed measured rounds are kept.** Each one records its class (`timeout`, `rate_limit`, `server`, `client`, `stream`, `protocol`, `io`, `config`), the wall time of the attempt that failed, and how many attempts the round used. Warmup failures stay outside the sample
- Rank and `--winner` use goodput: successful-round tokens/s × the success rate. A race with no failures ranks exactly as before. Latency, tokens/s and cost/run stay on the successful rounds and the report says so
- A retry that later succeeds increments `retry_attempts` and is not a failed round. Retries are counted and not priced, because a failed attempt does not return token usage

### Added

- `LaneSummary::failures`, `retry_attempts`, `measured_rounds`, `failure_rate` and `goodput_tokens_per_sec`
- `FailureClass` and `RoundFailure`
- Markdown, terminal and HTML tables gain a Fail column (`failed/measured`). JSON adds the same counts plus the failure list. Prometheus gains `llmrace_failure_rate`, `llmrace_goodput_tokens_per_second` and `llmrace_retry_attempts`

## [1.9.0] - 2026-11-20

Cost follows the cache split the provider already billed. A cache read was priced as a fresh input token, and Anthropic's `input_tokens` (which excludes cache reads and cache writes) was treated as the whole prompt.

### Fixed

- **Cache-aware cost.** OpenAI `prompt_tokens_details.cached_tokens` and Gemini `cachedContentTokenCount` are subsets of the reported prompt count and are priced at the published cached-input rate. Anthropic `input_tokens`, `cache_read_input_tokens` and `cache_creation_input_tokens` are added into the prompt total, with the 5-minute and 1-hour write buckets priced at 1.25× and 2× the base input rate for the Claude models in the table. Bedrock Converse `inputTokens` follows the same split. A usage event that only carries output tokens does not clear a cache count from `message_start`, and it still does not start the TTFB clock
- A model that reports cache tokens but has no cached price in the table is billed at the input rate, and the markdown report says so. Hourly cache storage is not in the per-request usage and is not included
- `--price` rejects non-finite values

### Added

- `StreamEvent` and `RunMetrics` carry `cache_read_tokens`, `cache_write_tokens` and `cache_write_1h_tokens`
- `cost::TokenRates`, `cost::UsageTokens`, `cost::token_rates`, `cost::usage_cost_usd` and `cost::parse_price_override`
- `--price provider/model=in:out[:cached[:write[:write_1h]]]`. A two-part override still means input and output only, and it no longer inherits a built-in cache discount
- JSON reports add the three cache counts on each run and lane, plus `cache_discount_applied`

## [1.8.0] - 2026-11-20

Cost uses the bill the provider sent. Input tokens were previously a
character estimate of the user prompt, so a system prompt or a tool schema
never appeared in the dollar figure.

### Fixed

- **Billed input tokens.** OpenAI `usage.prompt_tokens`, Anthropic `message_start` / `message_delta` `input_tokens`, Gemini `promptTokenCount`, Cohere billed input units, and Bedrock `usage.inputTokens` are kept on the run. `cost_per_run_usd` uses that count when any round reported it. A usage-only event does not start the TTFB clock
- Markdown cost cells that still fall back to the prompt-text estimate are marked with a trailing `~`. The estimate does not include a system prompt or tool schemas

### Added

- `StreamEvent::prompt_tokens`, `LaneSummary::mean_prompt_tokens` and `LaneSummary::prompt_tokens_measured`
- JSON reports add per-run `prompt_tokens` plus lane `prompt_tokens` and `prompt_tokens_measured`

## [1.7.0] - 2026-11-20

Reproducible races. Sampling is now part of the measurement, retries behave
like a production client, and the experimental cloud providers send real
request bodies instead of JSON `null`.

### Added

- **Reproducible sampling**: `--temperature`, `--top-p`, `--seed` and `--system`, plus the same keys on `llmrace.toml` profiles (integer temperatures are accepted). Unset controls are omitted, so a race that does not pass them still measures each provider's own default. Set controls are encoded in the wire's native shape — Cohere's `p`, Anthropic's top-level `system`, Gemini `generationConfig` / `systemInstruction`, Bedrock `inferenceConfig` — and stamped into the markdown, JSON, HTML and ramp reports. OpenAI reasoning models (gpt-5, o-series) keep the system prompt and drop temperature, top-p and seed, because those APIs reject the fields. Out-of-range values fail before the first request
- `Error::Http` carries an optional `retry_after` parsed from a `Retry-After` delay in seconds, and `Error::is_retryable` separates permanent client errors from timeouts, 429 and 5xx
- `provider::benchmark_client`: connect timeout capped at 10 seconds, idle pool sized to the race concurrency, TCP keepalive, and HTTP/2 when the server offers it
- Starter profile `repro` (`temperature = 0`, fixed seed, system prompt)

### Fixed

- **Bedrock and Vertex were sending `null` bodies.** Bedrock now calls Converse Stream — the operation the event-stream decoder already reads (`contentBlockDelta`, `metadata`) — with a real document, percent-encoded model id, `toolConfig` when `--tools` is set, and `Accept: application/vnd.amazon.eventstream`. Vertex now sends the Gemini `streamGenerateContent` document. Both remain experimental until a live account confirms the path ([#126](https://github.com/dfbustosus/llmrace/issues/126), [#127](https://github.com/dfbustosus/llmrace/issues/127))
- **`--retries` retried permanent failures.** A 400, 401, 403 or 404 now fails the round immediately. Retryable failures wait with full-jitter exponential backoff, and a `Retry-After` hint wins (capped at 30 seconds so a hostile header cannot stall the race)

### Changed

- The HTTP client negotiates HTTP/2 when the endpoint offers it, so a concurrency sweep against a cloud API multiplexes on one connection instead of one TCP connection per stream. HTTP/1.1 servers (local daemons) are unchanged. Saturation numbers against HTTP/2 providers can differ from 1.6.0 for that reason

## [1.6.0] - 2026-11-19

The platform release: llmrace grows from a benchmark CLI into a complete
provider evaluation and monitoring platform — define suites, measure with
statistics, gate CI on significance, monitor with alerts, and decide with
data-driven recommendations.

### New capabilities

- **Benchmark suites** (`--suite file.toml`): multiple named races with a combined report — different prompts, concurrency levels, token budgets, sweeps, tool tasks — as one command
- **Provider recommendation** (`--recommend`): analyzes accumulated history and answers "which provider should I use?" — ranked on speed, throughput, stability, cost, and value-for-money (tokens/s per $/M), with cost projections and `--json` for CI
- **Alert webhooks** (`--alert-webhook URL`): watch alerts page Slack/Discord (fire-and-forget, non-blocking); `--alert-test` verifies the pipeline
- **Desktop notifications** (`--notify-desktop`): native notifications on race completion and alerts (macOS/Linux/Windows)
- **Prometheus file export** (`--prom-output`): text-format metrics to a file for node_exporter textfile collector
- **`--version-json`**: machine-readable version info for CI scripts
- Suite entries support `sweep`, `tools`, and `tool_task`

### Quality improvements

- Suite combined reports show real data tables (was empty — run_race now returns LaneSummary results)
- Suite runner propagates all output flags (`--prom-output`, `--alert-webhook`, `--notify-desktop`, `--push-gateway`)
- Comparison table: 17 capability rows, every one a feature no competitor has
- Demo GIF re-recorded with current features (significance, recommendation, doctor)
- `--weights` errors include the actual invalid spec
- Pre-flight credential checks for experimental providers (bedrock/vertex fail immediately, not on first request)
- Vertex token + credentials cached process-wide (was re-authenticating per request)
- Amazon Bedrock provider (experimental): SigV4 + binary event-stream, vector-verified, fuzzed
- Google Vertex AI provider (experimental): service-account JWT RS256, Gemini SSE reuse


### Added

- `--recommend --json`: structured recommendation output for CI and tooling consumers — provider profiles with all metrics, costs, and value-for-money as machine-readable JSON
- `--alert-test`: sends a test alert to your `--alert-webhook` URL (and fires a desktop notification with `--notify-desktop`) so you can verify the pipeline before relying on it in production
- Suite entries support `sweep` (prompt-variant sweeps per suite) and `tools`/`tool_task` (tool-calling benchmarks per suite)

### Improved (professional polish)

- Comparison table expanded to 17 capability rows, including value-for-money, saturation curves, cross-region merging, reasoning honesty, and distribution channels — every row is a feature no competitor has
- Demo GIF re-recorded with the current binary: significance verdict, provider recommendation, and doctor diagnostics
- `--weights` errors now include the actual invalid spec (was just "parsing --weights")
- **Pre-flight credential checks**: `bedrock` and `vertex` fail immediately with the exact env vars needed, instead of starting the race and failing on the first request


### Added

- **`llmrace --recommend`** (enhanced): value-for-money axis (tokens/s per $/M tokens — the metric procurement actually needs), cost projections ("cheapest provider saves ≈$X/mo at 1k req/day"), and a helpful note listing unpriced providers with the exact `--price` command to fix them
- Suite runner now propagates `--prom-output`, `--alert-webhook`, `--notify-desktop`, and `--push-gateway` to each suite's races (previously silently dropped)
- Analyzes accumulated `--record` history and answers the question every benchmark leads to — *which provider should I use?* Ranks providers on four axes (fastest, highest throughput, most stable, cheapest) with a plain-language recommendation for latency-sensitive vs batch workloads. No competitor does this
- **Benchmark suites** (fixed): combined report now includes real data tables per suite (latency, TTFB, throughput, ITL, cost) — previously the tables were empty because the suite runner discarded LaneSummary results. `run_race` refactored to return `(exit_code, Vec<LaneSummary>)`: `llmrace --suite file.toml` runs multiple named races sequentially (different prompts, concurrency levels, token budgets) and produces a combined markdown report with one ranked table per suite. Suite entries support `providers`, `runs`, `warmup`, `concurrency`, `max_tokens`, `prompt`, and `prompt_file` — see `examples/llmrace.suite.toml`. Exit code reflects all suites (any failure → 1)
- **Watch alert webhooks**: `--alert-webhook URL` POSTs alerts (fire-and-forget, non-blocking) as JSON with both `text` (Slack) and `content` (Discord) fields — one URL serves either platform. Turns `--watch` into a production on-call tool: degraded providers page you through your existing alerting stack
- **Desktop notifications**: `--notify-desktop` sends native notifications (macOS Notification Center, Linux `notify-send`, Windows toast) on race completion ("mock won at 87 tok/s — 4 lanes raced") and on every watch alert — no terminal-watching required
- `--prom-output <file>`: writes Prometheus text-format metrics to a file after the race — no pushgateway needed. Pairs with node_exporter's textfile collector or any file-based scrape setup; complements the existing `--push-gateway`


## [1.5.1] - 2026-11-18

Vertex performance fix: per-request auth overhead eliminated (token
exchange, HTTP client creation, and credentials file re-read were all
happening on every streamed completion). No behavioral changes otherwise.


### Fixed

- **Vertex per-request overhead eliminated**: every streamed completion was doing a full OAuth2 token exchange (~500 ms), creating a fresh HTTP client, AND re-reading the credentials JSON from disk. Tokens are now cached process-wide (refreshed within 60 s of expiry), `fetch_token` reuses the caller's client, and the ServiceAccount file is read once per process (errors not cached, so fixes are picked up)


## [1.5.0] - 2026-11-17

The enterprise-cloud release: both remaining provider gaps — Amazon Bedrock
and Google Vertex AI — implemented with the same honest discipline (verified
crypto primitives, fuzzed parsers, experimental labels where live
verification is pending). Eighteen providers total.


### Added

- **Provider: Google Vertex AI** (`vertex`, **experimental**): service-account OAuth2 (JWT RS256 construction structurally tested against a real 2048-bit key; token exchange not yet live-verified — #127), reusing the Gemini SSE wire for streaming and `--models`. Credentials: `GOOGLE_APPLICATION_CREDENTIALS` + `GOOGLE_CLOUD_PROJECT` + `GOOGLE_CLOUD_REGION`. The RSA decision: RustCrypto's audited `rsa` crate (pure Rust, no unsafe) — vendoring big-integer RSA would be reckless
- `--models bedrock` calls the SigV4-signed ListFoundationModels API (AWS-confirmed signature format with fake creds → HTTP 403 "invalid security token")

### Fixed

- `--models bedrock` panicked (`unreachable!` in the discovery URL match)
- README comparison table: provider count refreshed (18 + custom)


### Fixed

- `--models bedrock` panicked (`unreachable!` in the discovery URL match); it now calls the SigV4-signed ListFoundationModels API — and with fake credentials AWS answers HTTP 403 "invalid security token", which live-confirms the signature *format* is well-formed even before real-account verification (#126)
- README comparison table: provider count refreshed (17 + custom)


### Added

- **Provider: Amazon Bedrock** (`bedrock`, **experimental**): SigV4 request signing and the AWS binary event-stream wire format, both implemented dependency-free (vendored SHA-256/HMAC/CRC-32C) and verified against published test vectors (FIPS 180-4, RFC 4231, iSCSI CRC-32C); the frame parser is a new fuzz target (148k clean runs locally). Credentials from `AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY`/`AWS_SESSION_TOKEN` + `AWS_REGION`. **The end-to-end path is not yet live-verified against a real account** — labeled experimental in `--list` and the README; reports welcome on #126
- Fuzz matrix now covers all three parser boundaries: SSE, provider JSON, event-stream frames


### Verified (post-1.3.2 acceptance)

- Scratch image raced a **live cloud API**: gpt-4o-mini at 167 tok/s from inside the 2.9 MB `FROM scratch` container — full streaming TLS path, not just mock
- GitHub Action self-test green against 1.3.2 (latest-path and pinned-version jobs)
- 40-second watch soak: alternating mock + local-Ollama rounds, `--alert-rounds 2` debounce armed, graceful SIGINT, both providers recorded to history and rendered in the final report


## [1.3.2] - 2026-11-16

Static everything: musl builds for linux amd64+arm64 (seven release
targets), a 2.9 MB `FROM scratch` Docker image, and the Bedrock/Vertex
contribution designs. No behavioral changes.


### Added

- **musl static builds** for linux amd64+arm64 (release matrix now 7 targets) and a `FROM scratch` Docker image — **2.9 MB total**, zero glibc surface, verified live in-container (race + TLS doctor); the ubuntu image remains as the `latest` tag, scratch ships as `:scratch`
- Provider design issues for **Amazon Bedrock** (#126: SigV4 signer testable against AWS's public vectors + the AWS binary event-stream parser) and **Google Vertex AI** (#127: service-account OAuth2, SSE decoding already done) — both `good first issue`, scoped for contributors with credentials
- `docs/launch/launch-kit.md`: Show HN + Reddit posts and launch checklist


## [1.3.1] - 2026-11-16

The reliability patch: two live-found correctness fixes, compile-time safety
guarantees, enforced contracts, two new distribution channels, and graceful
degradation everywhere. No new concepts to learn — everything gets more
honest or more robust.

### Fixed

- **Reports state the rounds actually measured**: `--duration` previously wrote its internal 100,000-round sentinel into markdown/JSON metadata (and a graceful interrupt would overstate too); metadata now reflects reality
- **Tool-mode reasoning attribution fixed** (live-found): streamed tool-argument tokens never counted toward the visible estimate, so classic models showed phantom `🧠` annotations in `--tools` mode (gpt-4o-mini: 🧠45); argument fragments and their characters now count, and the 🧠 annotation only renders for known reasoning models — live-verified: gpt-4o-mini clean at 53 tok/s, gpt-5-nano honestly 🧠499 in the same race
- **Two new distribution channels**: `dfbustosus/llmrace-action@v1` — a composite GitHub Action (one-line CI adoption, self-tested with zero-key mock and pinned-version jobs) — and `ghcr.io/dfbustosus/llmrace` multi-arch Docker images built on every release tag (glibc-matched ubuntu 24.04 base; verified locally through race/doctor in-container)
- Release matrix gains **Linux ARM64** (`aarch64-unknown-linux-gnu`, native runner) — the binary the ARM Docker image needs
- **CLI contract test**: the README's "flags are a contract" stability promise is now mechanically enforced — a test asserts all fifty documented flags exist in `--help`, so removing or renaming any of them fails CI
- `--help` is organized into sections (Race, Custom endpoint, Prompt shaping, Output, Scoring, CI gates, History & monitoring, Modes) — forty-plus flags were previously a single unstructured wall
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

[Unreleased]: https://github.com/dfbustosus/llmrace/compare/v1.39.0...HEAD
[1.39.0]: https://github.com/dfbustosus/llmrace/compare/v1.38.0...v1.39.0
[1.38.0]: https://github.com/dfbustosus/llmrace/compare/v1.37.2...v1.38.0
[1.37.2]: https://github.com/dfbustosus/llmrace/compare/v1.37.1...v1.37.2
[1.37.1]: https://github.com/dfbustosus/llmrace/compare/v1.37.0...v1.37.1
[1.37.0]: https://github.com/dfbustosus/llmrace/compare/v1.36.0...v1.37.0
[1.36.0]: https://github.com/dfbustosus/llmrace/compare/v1.35.1...v1.36.0
[1.35.1]: https://github.com/dfbustosus/llmrace/compare/v1.35.0...v1.35.1
[1.35.0]: https://github.com/dfbustosus/llmrace/compare/v1.34.0...v1.35.0
[1.34.0]: https://github.com/dfbustosus/llmrace/compare/v1.33.0...v1.34.0
[1.33.0]: https://github.com/dfbustosus/llmrace/compare/v1.32.1...v1.33.0
[1.32.1]: https://github.com/dfbustosus/llmrace/compare/v1.32.0...v1.32.1
[1.32.0]: https://github.com/dfbustosus/llmrace/compare/v1.31.0...v1.32.0
[1.31.0]: https://github.com/dfbustosus/llmrace/compare/v1.30.0...v1.31.0
[1.30.0]: https://github.com/dfbustosus/llmrace/compare/v1.29.0...v1.30.0
[1.29.0]: https://github.com/dfbustosus/llmrace/compare/v1.28.0...v1.29.0
[1.28.0]: https://github.com/dfbustosus/llmrace/compare/v1.27.0...v1.28.0
[1.27.0]: https://github.com/dfbustosus/llmrace/compare/v1.26.0...v1.27.0
[1.26.0]: https://github.com/dfbustosus/llmrace/compare/v1.25.1...v1.26.0
[1.25.1]: https://github.com/dfbustosus/llmrace/compare/v1.25.0...v1.25.1
[1.25.0]: https://github.com/dfbustosus/llmrace/compare/v1.24.0...v1.25.0
[1.24.0]: https://github.com/dfbustosus/llmrace/compare/v1.23.0...v1.24.0
[1.23.0]: https://github.com/dfbustosus/llmrace/compare/v1.22.0...v1.23.0
[1.22.0]: https://github.com/dfbustosus/llmrace/compare/v1.21.0...v1.22.0
[1.21.0]: https://github.com/dfbustosus/llmrace/compare/v1.20.0...v1.21.0
[1.20.0]: https://github.com/dfbustosus/llmrace/compare/v1.19.0...v1.20.0
[1.19.0]: https://github.com/dfbustosus/llmrace/compare/v1.18.0...v1.19.0
[1.18.0]: https://github.com/dfbustosus/llmrace/compare/v1.17.0...v1.18.0
[1.17.0]: https://github.com/dfbustosus/llmrace/compare/v1.16.0...v1.17.0
[1.16.0]: https://github.com/dfbustosus/llmrace/compare/v1.15.0...v1.16.0
[1.15.0]: https://github.com/dfbustosus/llmrace/compare/v1.14.0...v1.15.0
[1.14.0]: https://github.com/dfbustosus/llmrace/compare/v1.13.0...v1.14.0
[1.13.0]: https://github.com/dfbustosus/llmrace/compare/v1.12.0...v1.13.0
[1.12.0]: https://github.com/dfbustosus/llmrace/compare/v1.11.0...v1.12.0
[1.11.0]: https://github.com/dfbustosus/llmrace/compare/v1.10.0...v1.11.0
[1.10.0]: https://github.com/dfbustosus/llmrace/compare/v1.9.0...v1.10.0
[1.9.0]: https://github.com/dfbustosus/llmrace/compare/v1.8.0...v1.9.0
[1.8.0]: https://github.com/dfbustosus/llmrace/compare/v1.7.0...v1.8.0
[1.7.0]: https://github.com/dfbustosus/llmrace/compare/v1.6.0...v1.7.0
[0.5.0]: https://github.com/dfbustosus/llmrace/compare/v0.4.2...v0.5.0
[0.4.2]: https://github.com/dfbustosus/llmrace/compare/v0.4.1...v0.4.2
[0.4.1]: https://github.com/dfbustosus/llmrace/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/dfbustosus/llmrace/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/dfbustosus/llmrace/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/dfbustosus/llmrace/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/dfbustosus/llmrace/releases/tag/v0.1.0
