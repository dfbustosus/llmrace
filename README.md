<div align="center">

# 🏁 llmrace

**Race LLM providers head-to-head in your terminal.**

Latency · Time-to-first-byte · Tokens/s · p95 · Cost — live, in one command.

[![CI](https://img.shields.io/github/actions/workflow/status/dfbustosus/llmrace/ci.yml?style=flat-square&logo=githubactions&label=CI)](https://github.com/dfbustosus/llmrace/actions)
[![Crates.io](https://img.shields.io/crates/v/llmrace?style=flat-square&logo=rust&color=3390FF)](https://crates.io/crates/llmrace)
[![Codecov](https://img.shields.io/codecov/c/github/dfbustosus/llmrace?style=flat-square&logo=codecov&color=3390FF)](https://codecov.io/gh/dfbustosus/llmrace)
[![License](https://img.shields.io/crates/l/llmrace?style=flat-square&color=8B949E)](#license)
[![Made with Rust](https://img.shields.io/badge/built%20with-Rust-DEA584?style=flat-square&logo=rust)](https://www.rust-lang.org)
[![MSRV](https://img.shields.io/badge/MSRV-1.88-DEA584?style=flat-square&logo=rust)](./Cargo.toml)

*A single static binary. No telemetry. Works with cloud APIs and local models.*

</div>

---

## Why

Every LLM provider claims to be "fast". **TTFB, streaming throughput and p95 tail latency tell a different story** depending on your region, your model and your prompt length. `llmrace` measures it for real: it fires the *same streaming request* at every provider **simultaneously**, watches the tokens arrive live, and ranks them.

## Contents

- [Why](#why) · [Demo](#demo) · [Install](#install) · [Quick start](#quick-start)
- [CI quality gate](#using-llmrace-as-a-ci-quality-gate) · [Distributed probes](#distributed-probes) · [Results](#real-world-results)
- [Tracking over time](#tracking-providers-over-time) · [Diagnosing](#diagnosing-your-setup) · [Pricing](#pricing)
- [What it measures](#what-it-measures) · [Reports](#reports) · [Comparison](#how-it-compares)
- [Engineering](#engineering) · [Contributing](#contributing) · [License](#license)

## Demo

![llmrace demo](docs/demo.gif)

*A multi-model race — classic `gpt-4o-mini` vs local Ollama and the instant mock lane — with the `--winner` podium and Welch significance verdict, then `--recommend` (provider recommendation with value-for-money from accumulated history) and `--doctor` diagnostics. Reproducible via [`docs/demo.tape`](docs/demo.tape) ([vhs](https://github.com/charmbracelet/vhs); needs `OPENAI_API_KEY` in the environment).*

## Why

```text
 🏁 llmrace   run 1/3 ·  4.2s · "Write one vivid paragraph about a lighthouse at dawn."
┌─ GROQ ────────────────────────────────────────────────────────────────┐
 │ 🏁 412 ms   184 tok  ·  TTFB 88 ms                                   │
 │ `llama-3.3-70b-versatile`                                            │
 │ ████████████████████████████████████████░░░░░░░░░░░░░░░              │
└───────────────────────────────────────────────────────────────────────┘
┌─ OPENAI ──────────────────────────────────────────────────────────────┐
 │ ▶ streaming…   61 tok  ·  TTFB 210 ms                                │
 │ `gpt-4o-mini`                                                        │
 │ ██████████████░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░             │
└───────────────────────────────────────────────────────────────────────┘
┌─ OLLAMA (local) ──────────────────────────────────────────────────────┐
 │ ▶ streaming…   23 tok  ·  TTFB 41 ms                                 │
 │ `llama3.2`                                                           │
 │ █████░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░        │
└───────────────────────────────────────────────────────────────────────┘
q/Esc quit · report written on exit
```

## Install

**Homebrew (macOS & Linux):**

```bash
brew install dfbustosus/tap/llmrace
```

**Prebuilt binary** (no Rust toolchain needed):

```bash
cargo binstall llmrace
```

Or grab a binary for your platform from the [releases page](https://github.com/dfbustosus/llmrace/releases) — Linux (x86_64), macOS (Apple Silicon & Intel), Windows (x86_64), each with SHA-256 checksums.

**Docker** (no Rust, no Homebrew needed):

```bash
docker run --rm -e OPENAI_API_KEY ghcr.io/dfbustosus/llmrace openai -n 5
# 3 MB FROM-scratch static image (musl):
docker run --rm -e OPENAI_API_KEY ghcr.io/dfbustosus/llmrace-scratch:scratch openai -n 5
# reports land in /report — mount a volume to keep them:
docker run --rm -v "$PWD:/report" -e OPENAI_API_KEY ghcr.io/dfbustosus/llmrace openai --json
```

**GitHub Actions** (one line in any workflow):

```yaml
- uses: dfbustosus/llmrace-action@v1
  with:
    providers: openai groq
    args: "-n 5 --winner --json"
  env:
    OPENAI_API_KEY: ${{ secrets.OPENAI_API_KEY }}
    GROQ_API_KEY: ${{ secrets.GROQ_API_KEY }}
```

**Shell completions:**

```bash
llmrace --completions zsh > ~/.zfunc/_llmrace   # or bash/fish/powershell/elvish
```

**From crates.io:**

```bash
cargo install llmrace
```

**From source:**

```bash
git clone https://github.com/dfbustosus/llmrace
cd llmrace && cargo install --path .
```

## Quick start

```bash
llmrace --init    # create a starter llmrace.toml with example profiles
```

```bash
# Tool-calling: race argument validity (tasks: weather, math, lookup)
llmrace openai groq --tools math
```

```bash
# Race every provider you have configured (auto-detected from env vars)
llmrace

# Race specific providers
llmrace openai anthropic groq

# Include your local Ollama models
llmrace ollama groq -m llama-3.3-70b-versatile

# Mixed race: different models per provider (cloud + local)
llmrace openai ollama -m gpt-4o-mini -m ollama=llama3.2:1b

# Same provider, several models, head-to-head in one race
llmrace openai=gpt-4o-mini openai=gpt-4.1 ollama=llama3.2:1b --winner

# Saturation throughput: 8 parallel requests per provider
llmrace openai groq -c 8 -n 5

# Saturation curve: race at each concurrency level, find the throughput knee
llmrace openai groq --ramp 1,2,4,8 -n 3

# Time-bounded runs keep every round in memory for statistics
# (~2 KB/round/lane: inter-token gaps dominate). A 24 h --duration run at
# one round per lane every ~2 s holds roughly 100 MB per lane — for
# horizon monitoring prefer --watch, which aggregates each round instead.

# CI gate: fail if any provider exceeds 500ms TTFB
llmrace openai groq --max-ttfb 500 --quiet

# Reproducible race: pin sampling and stamp it into the report.
# Omit the flags to keep each provider's own default.
llmrace openai groq --temperature 0 --top-p 1 --seed 1 --system "Be concise."

# Declare an overall winner (weighted ttfb/tps/p95/cost composite)
llmrace openai groq --winner
llmrace openai groq --winner --weights ttfb=50,tps=50
llmrace openai groq --winner --weights tps=60,cost=40   # price-aware podium

# Retry failed rounds up to 3 times (flaky providers, CI runs)
llmrace openai groq --retries 3

# A/B compare two saved reports (no race, prints diff table)
llmrace --compare old.json,new.json

# Prompt-variant sweep: same race across prompt shapes
llmrace openai groq --sweep short,long,code,json,reasoning

# Custom variants (define your own prompts in llmrace.toml)
llmrace openai groq --sweep sql, summarizes

# Race ANY OpenAI-compatible endpoint (vLLM, LiteLLM, Together…)
llmrace --endpoint http://localhost:8000/v1/chat/completions \
        --provider-name vllm --api-key-env VLLM_API_KEY \
        openai groq

# Markdown to stdout (pipe into CI step summaries), JSON alongside
llmrace openai groq -n 5 --out - --json > "$GITHUB_STEP_SUMMARY"

# Benchmark suite: multiple races, one combined report
llmrace --suite examples/llmrace.suite.toml

# Custom prompt (inline or from a file), JSON + CSV output for CI
llmrace openai groq -p "Explain transformer attention in 3 sentences." -n 5 --json --csv
llmrace openai groq --prompt-file prompts/attention.txt

# Same workload on every lane: a race, a suite entry, or a ramp
llmrace openai groq --workload examples/workloads/short-answer.jsonl --ramp 1,2,4
```

Every provider with a matching env var races automatically — no config file:

| Provider | Env var | Default model |
|----------|---------|---------------|
| `openai` | `OPENAI_API_KEY` | `gpt-4o-mini` |
| `anthropic` | `ANTHROPIC_API_KEY` | `claude-3-5-haiku-latest` |
| `gemini` | `GEMINI_API_KEY` | `gemini-3.5-flash-lite` |
| `groq` | `GROQ_API_KEY` | `llama-3.3-70b-versatile` |
| `deepseek` | `DEEPSEEK_API_KEY` | `deepseek-chat` |
| `mistral` | `MISTRAL_API_KEY` | `mistral-small-latest` |
| `together` | `TOGETHER_API_KEY` | `Llama-3.3-70B-Instruct-Turbo` |
| `fireworks` | `FIREWORKS_API_KEY` | `llama-v3p3-70b-instruct` |
| `cerebras` | `CEREBRAS_API_KEY` | `llama-3.3-70b` |
| `deepinfra` | `DEEPINFRA_API_KEY` | `meta-llama/Meta-Llama-3.1-8B-Instruct` |
| `hyperbolic` | `HYPERBOLIC_API_KEY` | `meta-llama/Meta-Llama-3.1-70B-Instruct` |
| `xai` | `XAI_API_KEY` | `grok-3-mini` |
| `perplexity` | `PERPLEXITY_API_KEY` | `sonar` |
| `cohere` | `CO_API_KEY` | `command-a-03-2025` |
| `openrouter` | `OPENROUTER_API_KEY` | `openai/gpt-4o-mini` |
| `ollama` | — (local) | `llama3.2` |
| `bedrock` | `AWS_ACCESS_KEY_ID` + `AWS_SECRET_ACCESS_KEY` + `AWS_REGION` | `anthropic.claude-3-5-haiku-20241022-v1:0` — **experimental**¹ |
| `vertex` | `GOOGLE_APPLICATION_CREDENTIALS` + `GOOGLE_CLOUD_PROJECT` + `GOOGLE_CLOUD_REGION` | `gemini-3.5-flash-lite` — **experimental**¹ |
| `lmstudio` | — (opt-in local, port 1234) | loaded model |
| `llamacpp` | — (opt-in local, port 8080) | loaded model |
| azure | `AZURE_OPENAI_API_KEY` (library: `ProviderSpec::azure_openai`) | per deployment |
| custom | `--endpoint` + `--api-key-env` | any OpenAI-compatible server |

Opt-in local daemons (`lmstudio`, `llamacpp`) only race when named explicitly, so they never pollute auto-detected runs with error lanes.

¹ Bedrock speaks the Converse Stream API (one request body for every model family) and Vertex speaks Gemini `streamGenerateContent`. SigV4, the event-stream decoder and the Vertex JWT are verified against published test vectors. The Gemini and Vertex default model id is `gemini-3.5-flash-lite`. On 2026-12-01 Google routes `gemini-2.5-flash` to that id and bills it at the Flash-Lite rate. The live account path has still not been run here — no AWS or Google credentials were available — so both stay experimental ([#126](https://github.com/dfbustosus/llmrace/issues/126), [#127](https://github.com/dfbustosus/llmrace/issues/127)).

## What it measures

| Metric | Definition |
|--------|------------|
| **TTFT** | Time to first token. Same clock as TTFB: from just before the request is sent to the first streamed token (includes connection, TLS and queueing). Reports print TTFT; JSON keeps `mean_ttfb_ms` and adds `ttft_ms` |
| **TPOT** | Time per output token: the effective generation window divided by completion tokens. JSON reports the median (`tpot_p50_ms`) and p95 (`tpot_p95_ms`) across rounds |
| **Saturation peak** | `--ramp` races at each concurrency level and marks the concurrency where goodput peaks (⬇ flags a goodput drop of more than 10% past it). JSON names the first level whose failure rate crosses 5% |
| **Tokens/s** | Completion tokens divided by the *effective generation window* (total − TTFB, widened to the provider-reported server processing time when longer); with `--concurrency N`, total tokens of all N parallel requests over the round window — i.e. saturation throughput |
| **ITL p50 / p95** | Distribution of inter-token gaps while streaming — how *smooth* the stream feels, not just how fast it finishes |
| **Stutters** | Inter-token gaps over 250 ms — visible streaming hiccups, counted per lane |
| **Latency p50 / p95** | Distribution of round time-to-last-token (linear-interpolation percentiles) |
| **Mean ± 95% CI** | Student-t confidence interval on the mean latency |
| **Significance** | Welch's t-test on per-run latencies between the top two lanes — the report states whether the gap is real or could be noise (p < 0.05) |
| **Cost/run** | Public per-model prices × billed input and output tokens. Input tokens come from the provider's usage report (system prompt and tool schemas included). Cache reads and cache writes use the published discount when this crate has one for the model; a model with cache tokens but no cached price is billed at the input rate and the report says so. Hourly cache storage is not in the per-request usage and is not included. When a provider omits usage, the user prompt is estimated and the markdown table marks the figure with `~` |

Methodology notes:

- One **warmup round** per provider (excluded from stats) to absorb connection setup and TLS handshakes. A warmup failure is not a measured failure
- **Failed measured rounds stay in the report.** Latency, tokens/s and cost/run use successful rounds only. The rank uses goodput (tokens/s × the success rate), so a provider that timed out half its calls cannot win on the calls that happened to succeed. Retries are counted and not priced: a failed attempt does not return token usage. `--record`, `--baseline` and `--recommend` keep the same failure rate, goodput, retry count and cost. A history file or baseline written before those fields existed still loads; the gate skips a metric the baseline did not record
- Rounds are **concurrent across providers** but sequential within a provider, so providers don't throttle each other. Exception: `provider=model` lanes of the *same* provider race concurrently — the provider's rate limits and capacity are then shared between those lanes, which is exactly what a mixed-model race measures in practice
- Rounds with 3σ latency outliers are flagged with `†`
- **ITL caveat**: inter-token gaps are timestamped when the runtime polls the event, so they include local scheduling jitter (sub-millisecond on a quiet machine, more under heavy concurrent load) — compare ITL across providers measured in the *same* race, not across machines
- Token counts prefer **provider-reported usage** (`include_usage` on OpenAI-compatible APIs, `message_delta` on Anthropic) and fall back to stream-event counting
- `--workload` is a JSONL file (`id`, `prompt`, optional `expect`). Every lane draws the same prompt on a given round, in order, and wraps. The report adds a per-prompt table so a slow prompt is visible. `--suite` entries set `workload` to the same file. `--prompt-file` is still one prompt
- **Pass** is task successes over measured rounds. `--tools` checks that the tool arguments are valid. `expect` checks an exact short answer (the trimmed text, a JSON object, or a number). A wrong answer is still a successful request, and `--winner` can rank it last. A race with no task leaves Pass blank and the podium weights unchanged
- All requests in a run share `max_tokens` and sampling parameters. Omit a workload and they also share one prompt. Omit `--temperature`, `--top-p`, `--seed` and `--system` to keep each provider's own default. When you set them, the values are stamped into the markdown, JSON and HTML reports. OpenAI reasoning models (gpt-5, o-series) ignore temperature, top-p and seed, because those APIs reject the fields; the system prompt is still sent
- Per-request timeouts (`--timeout`) are enforced mid-stream; panics in a single request never abort the race
- **Reasoning models** (gpt-5, o-series): TTFT honestly includes thinking time — the model emits nothing visible while it reasons. When the provider reports server processing time, tokens/s uses the wider of the observed streaming window and the server window, so burst delivery after thinking cannot inflate throughput ([#125](https://github.com/dfbustosus/llmrace/issues/125)). Gemini `thoughtsTokenCount` is billed as output on top of `candidatesTokenCount`. OpenAI `completion_tokens` already includes `reasoning_tokens`, so that count is recorded and not added again. When the provider sends no count, the visible-stream estimate remains

## Reports

```bash
llmrace openai groq --out race.md --json --csv --html
```

- `race.md` — ranked table with **mean ± 95% CI**, p50/p95, TTFB, tokens/s and cost, ready to paste into GitHub issues or blog posts
- `race.json` — full per-run data, confidence intervals and outliers for CI pipelines. It names `pricing_as_of`, the workload ids in measured-round order (empty for one prompt), and `prompt_sha256` of the prompt texts those rounds sent, joined with a newline
- `race.csv` — per-run rows for spreadsheets and dashboards
- `race.html` — self-contained dark-theme page with podium, ranked table and per-run latency bars (TTFB vs generation split), shareable as a single file

## Distributed probes

Run region-distributed probes from any workflow with the reusable [probe workflow](.github/workflows/probe.yml):

```yaml
jobs:
  probe:
    uses: dfbustosus/llmrace/.github/workflows/probe.yml@main
    with:
      providers: groq openai
      runs: 5
    secrets: inherit
```

Each run stamps its region into the report (`--region`) and uploads a region-tagged JSON artifact. Download the artifacts and compare regions — the fastest region per model is marked ⚡ (TTFB) and 🚀 (throughput):

```bash
llmrace --merge probe-São\ Paulo.json,probe-Virginia.json,probe-Frankfurt.json
```

Add `--json` to also write a machine-readable merged report.

## Diagnosing your setup

```bash
llmrace --doctor            # keys, endpoint egress, Ollama, config, history
llmrace --doctor --json     # structured output for CI
llmrace --list --json       # full provider registry with availability
llmrace --models groq       # models the provider currently offers
llmrace --models myvllm --endpoint http://localhost:8000/v1/chat/completions --provider-name myvllm
llmrace --schema > schema.json   # JSON Schema for race.json (CI validation)
```

Any HTTP response counts as endpoint reachability — even a 401 proves DNS, TLS and routing work, so `--doctor` distinguishes "missing key" from "network blocked".

## Pricing

Cost columns use a built-in table. OpenAI, Anthropic and Gemini rows were read on 2026-11-20 from [OpenAI pricing](https://developers.openai.com/api/docs/pricing), [Anthropic pricing](https://platform.claude.com/docs/en/about-claude/pricing) and [Gemini pricing](https://ai.google.dev/gemini-api/docs/pricing). Those rows include the models people race now (`gpt-5.4` and its mini and nano, `claude-sonnet-4-6`, `claude-haiku-4-5`, `gemini-2.5-flash`). `gemini-3.5-flash-lite` was read on 2026-11-21 from the same Gemini page ($0.30 / $2.50, cache read $0.03). The table is not a row for every model id. Other providers were not re-read that day. Cache reads and cache writes use the published discount for models whose cached rate is in the table. Override per model (repeatable, USD per million tokens). A two-part override replaces the whole row and does not keep the built-in cache discount; add `:cached`, `:write` and `:write_1h` when you want those rates:

```bash
llmrace openai --price openai/gpt-4o-mini=1.25:5
llmrace openai --price openai/gpt-4o-mini=0.15:0.60:0.075
```

## Tracking providers over time

Record every race to a local history file (JSONL under `~/.local/share/llmrace/`):

```bash
# Prometheus metrics to a file (no pushgateway needed)
llmrace openai groq --prom-output /path/to/textfile.collector/llmrace.prom

llmrace openai groq --record
```

Then analyze drift — TTFB/throughput changes versus the rolling median, with model-swap annotations:

```bash
llmrace --trend              # last 30 days, all providers
llmrace --trend --days 7 groq
llmrace --recommend           # which provider should I use?
llmrace --recommend --json    # structured output for CI
```

**Continuous monitoring with alerts:**

```bash
# watch every 15 min, alert after 2 consecutive degraded rounds,
# page Slack/Discord when providers degrade
llmrace openai groq --watch --every 900 --alert-rounds 2 \
  --alert-webhook https://hooks.slack.com/services/... --record

# desktop notification when the race completes (macOS/Linux/Windows)
llmrace openai groq --notify-desktop
```

## Using llmrace as a CI quality gate

Record a baseline once, then fail your pipeline when a provider regresses:

```bash
# 1. Record the baseline (commit llmrace-report.json to your repo)
llmrace openai groq --json --out llmrace-report.json

# 2. In CI: race again and compare — exits 1 on regression
llmrace openai groq --baseline llmrace-report.json --max-regression 15
```

The check compares mean latency, p95 and TTFT (increases) and tokens/s (decreases) against the threshold. When the baseline recorded them, it also fails if the failure rate rises by more than the threshold in percentage points, if goodput or task success falls by more than the threshold, or if cost/run rises by more than the threshold. A baseline that omits those fields skips them. A race with no task has no task-success metric, so a plain latency gate does not move. The report prints an old → new diff table with per-metric Welch p-values, and flags providers that are new or missing from the baseline. With `--require-significance`, a latency or tokens/s breach fails the gate only when the difference is statistically significant (p < 0.05 on per-run data). Failure rate, goodput, cost and task success are ratios of the whole sample, so a threshold breach on those counts without a t-test:

```bash
llmrace openai groq --baseline llmrace-report.json --max-regression 15 --require-significance
```

A ready-to-copy workflow lives in [`docs/example-latency-gate.yml`](docs/example-latency-gate.yml). With `--sweep`, each variant lane compares against that provider's matching variant baseline entry, falling back to a plain (variant-less) entry. A run where **every** provider failed exits non-zero — a green pipeline over a total outage is never silent.

## Real-world results

Measured 2026-11-15 from South America — three OpenAI models head-to-head vs local inference (Ollama on Apple M5 Pro / 48 GB), 5 rounds each, warmup excluded:

| Provider | Model | TTFB | p95 | Tokens/s | ITL p95 | Notes |
|---|---|---:|---:|---:|---:|---|
| openai | `gpt-4o-mini` | 625 ms | 1647 ms | **209.9** | 14 ms | fastest all-round |
| openai | `gpt-5-nano` | 1778 ms | 1870 ms | 163.2 | — | 🧠 1275 hidden reasoning tokens |
| openai | `gpt-4.1` | 548 ms | 2416 ms | 122.4 | 29 ms | long generations, spikier ITL |
| ollama | `llama3.2:1b` | 109 ms | 2697 ms | 75.6 | 18 ms | local, ~0 network |

And the statistic llmrace exists to surface: **`⚖ gpt-4o-mini vs gpt-5-nano: Δ −351 ms, significant (p=0.010)`** — the classic model really is faster end-to-end, while gpt-5-nano spends 1.8 s thinking before its first visible token (🧠 1275 of its token budget). Reproduce:

```bash
llmrace openai=gpt-4o-mini openai=gpt-4.1 openai=gpt-5-nano -n 5 --warmup 1 --winner
```

PRs with results from other regions are welcome — record with `--json` and extend this table.

## How it compares

| | llmrace | provider CLIs | load testers (k6/vegeta) | router dashboards |
|---|---|---|---|---|
| Multi-provider streaming race | ✅ 18 + any OpenAI-compatible | one at a time | ✝ hand-written scripts | ❌ passive |
| Statistical significance (Welch p-values) | ✅ built-in | ❌ | ❌ | ❌ |
| Provider recommendation from history | ✅ `--recommend` | ❌ | ❌ | ❌ |
| Value-for-money (tok/s per $/M) | ✅ built-in | ❌ | ❌ | ❌ |
| Benchmark suites (multi-race batteries) | ✅ `--suite` | ❌ | ✝ heavy scripting | ❌ |
| Alert webhooks (Slack/Discord) | ✅ `--alert-webhook` | ❌ | ✝ custom | ❌ |
| Desktop notifications | ✅ `--notify-desktop` | ❌ | ❌ | ❌ |
| TTFB / tokens/s / p95 / t-CI / ITL | ✅ built-in | varies | ✝ you build it | ✅ but only routed traffic |
| Failed rounds kept; rank is goodput (tokens/s × success rate) | ✅ | ❌ | partial | partial |
| Reasoning-model honesty (🧠 split) | ✅ built-in | ❌ | ❌ | ❌ |
| Live terminal race view | ✅ | ❌ | ❌ | ❌ |
| CI regression gate with significance | ✅ `--require-significance` | ❌ | ✝ heavy scripting | ❌ |
| Saturation curves (`--ramp`) | ✅ built-in | ❌ | ✝ complex | ❌ |
| Longitudinal trends + model-swap detection | ✅ | ❌ | ❌ | partial |
| Cost per run from billed input, cached input and output tokens | ✅ | ❌ | ❌ | partial |
| Cross-region probe merging | ✅ `--merge` | ❌ | ❌ | ❌ |
| Reproducible sampling (temperature, top-p, seed, system) stamped on the report | ✅ | ❌ | ✝ | ❌ |
| Retries honor `Retry-After`; permanent 4xx fails fast | ✅ | ❌ | partial | ❌ |
| Single static binary, no config file | ✅ | ❌ | ❌ | ❌ |
| 100% safe Rust, fuzzed parsers | ✅ | — | — | — |
| Docker (2.9 MB scratch) + GitHub Action | ✅ | ❌ | partial | ❌ |

✝ possible with effort — llmrace exists so you don't spend that effort.
Missing a capability you need? [Open an issue](https://github.com/dfbustosus/llmrace/issues/new/choose).

## Engineering

- `#![deny(missing_docs)]`, `#![deny(clippy::unwrap_used)]` and `#![forbid(unsafe_code)]` — **100% safe Rust, enforced at compile time**; the binary's runtime paths contain zero `unwrap`/`expect` calls
- Typed errors via `thiserror`; `anyhow` only at the binary edge
- Zero-alignment-allocation SSE parser that survives arbitrary network chunk boundaries (exhaustively tested at every split point)
- The race HTTP client sizes its idle pool to the concurrency, caps the connect timeout at 10s, and keeps TCP and HTTP/2 connections alive so warmup is the round that pays for the handshake
- Criterion benchmarks for the parsing and statistics hot paths
- Panic containment: a failing request becomes an error row, never a crash
- CI on Linux/macOS/Windows: fmt + clippy `-D warnings` + tests + end-to-end mock race

## Stability

**From 1.0.0 the surface is frozen:**

- **CLI**: flags are a contract — deprecations get one minor release of dual support before removal
- **Library**: semver — breaking API changes only in a new major version; new fields arrive only additively in minors
- **MSRV**: Rust 1.88, enforced in CI; MSRV bumps are minor-release changes announced in the changelog
- **Reports**: `race.json` / `race.csv` field changes are additive-only; `--baseline` and `--compare` keep parsing older reports (written since 0.3)

## Roadmap

- **v1.x** — more providers, richer reasoning-model metrics (usage vs visible token split, reasoning-time breakdown), community results across regions
- **Stability freeze is in effect** — new features land additively

Most provider additions are tagged [`good first issue`](https://github.com/dfbustosus/llmrace/issues?q=is%3Aissue+is%3Aopen+label%3A%22good+first+issue%22) — grab one.

## Contributing

PRs welcome — adding a provider is ~40 lines (see `src/provider.rs`). Fork it, add your favorite provider, ship it.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE) at your option.
