<div align="center">

# 🏁 llmrace

**Race LLM providers head-to-head in your terminal.**

Latency · Time-to-first-byte · Tokens/s · p95 · Cost — live, in one command.

[![CI](https://img.shields.io/github/actions/workflow/status/dfbustosus/llmrace/ci.yml?style=flat-square&logo=githubactions&label=CI)](https://github.com/dfbustosus/llmrace/actions)
[![Crates.io](https://img.shields.io/crates/v/llmrace?style=flat-square&logo=rust&color=3390FF)](https://crates.io/crates/llmrace)
[![Codecov](https://img.shields.io/codecov/c/github/dfbustosus/llmrace?style=flat-square&logo=codecov&color=3390FF)](https://codecov.io/gh/dfbustosus/llmrace)
[![License](https://img.shields.io/crates/l/llmrace?style=flat-square&color=8B949E)](#license)
[![Made with Rust](https://img.shields.io/badge/built%20with-Rust-DEA584?style=flat-square&logo=rust)](https://www.rust-lang.org)

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

*A multi-model race — classic `gpt-4o-mini` vs the reasoning `gpt-5-nano` (🧠 hidden-reasoning tokens, honest throughput) against local Ollama and the instant mock lane — finishing in the ranked table and the `--winner` podium, then live model discovery (`--models`) and `--doctor` diagnostics. Reproducible via [`docs/demo.tape`](docs/demo.tape) ([vhs](https://github.com/charmbracelet/vhs); needs `OPENAI_API_KEY` in the environment).*

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

# CI gate: fail if any provider exceeds 500ms TTFB
llmrace openai groq --max-ttfb 500 --quiet

# Declare an overall winner (weighted ttfb/tps/p95 composite)
llmrace openai groq --winner
llmrace openai groq --winner --weights ttfb=50,tps=50

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

# Custom prompt (inline or from a file), JSON + CSV output for CI
llmrace openai groq -p "Explain transformer attention in 3 sentences." -n 5 --json --csv
llmrace openai groq --prompt-file prompts/attention.txt
```

Every provider with a matching env var races automatically — no config file:

| Provider | Env var | Default model |
|----------|---------|---------------|
| `openai` | `OPENAI_API_KEY` | `gpt-4o-mini` |
| `anthropic` | `ANTHROPIC_API_KEY` | `claude-3-5-haiku-latest` |
| `gemini` | `GEMINI_API_KEY` | `gemini-2.0-flash` |
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
| `lmstudio` | — (opt-in local, port 1234) | loaded model |
| `llamacpp` | — (opt-in local, port 8080) | loaded model |
| azure | `AZURE_OPENAI_API_KEY` (library: `ProviderSpec::azure_openai`) | per deployment |
| custom | `--endpoint` + `--api-key-env` | any OpenAI-compatible server |

Opt-in local daemons (`lmstudio`, `llamacpp`) only race when named explicitly, so they never pollute auto-detected runs with error lanes.

## What it measures

| Metric | Definition |
|--------|------------|
| **TTFB** | Time from just before the request is sent to the first streamed token (includes connection, TLS and queueing) |
| **Saturation peak** | `--ramp` races at each concurrency level and marks each provider's peak throughput (⬇ flags >10% degradation past it) |
| **Tokens/s** | Completion tokens divided by the *effective generation window* (total − TTFB, widened to the provider-reported server processing time when longer); with `--concurrency N`, total tokens of all N parallel requests over the round window — i.e. saturation throughput |
| **ITL p50 / p95** | Distribution of inter-token gaps while streaming — how *smooth* the stream feels, not just how fast it finishes |
| **Stutters** | Inter-token gaps over 250 ms — visible streaming hiccups, counted per lane |
| **Latency p50 / p95** | Distribution of round time-to-last-token (linear-interpolation percentiles) |
| **Mean ± 95% CI** | Student-t confidence interval on the mean latency |
| **Significance** | Welch's t-test on per-run latencies between the top two lanes — the report states whether the gap is real or could be noise (p < 0.05) |
| **Cost/run** | Provider-reported token counts × public per-model prices (editable in `src/cost.rs`) |

Methodology notes:

- One **warmup round** per provider (excluded from stats) to absorb connection setup and TLS handshakes
- Rounds are **concurrent across providers** but sequential within a provider, so providers don't throttle each other. Exception: `provider=model` lanes of the *same* provider race concurrently — the provider's rate limits and capacity are then shared between those lanes, which is exactly what a mixed-model race measures in practice
- Rounds with 3σ latency outliers are flagged with `†`
- Token counts prefer **provider-reported usage** (`include_usage` on OpenAI-compatible APIs, `message_delta` on Anthropic) and fall back to stream-event counting
- All requests use the same prompt, `max_tokens` and sampling defaults per run
- Per-request timeouts (`--timeout`) are enforced mid-stream; panics in a single request never abort the race
- **Reasoning models** (gpt-5, o-series): TTFB honestly includes thinking time — the model emits nothing visible while it reasons. When the provider reports server processing time, tokens/s uses the wider of the observed streaming window and the server window, so burst delivery after thinking cannot inflate throughput ([#125](https://github.com/dfbustosus/llmrace/issues/125))

## Reports

```bash
llmrace openai groq --out race.md --json --csv --html
```

- `race.md` — ranked table with **mean ± 95% CI**, p50/p95, TTFB, tokens/s and cost, ready to paste into GitHub issues or blog posts
- `race.json` — full per-run data, confidence intervals and outliers for CI pipelines
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
llmrace --schema > schema.json   # JSON Schema for race.json (CI validation)
```

Any HTTP response counts as endpoint reachability — even a 401 proves DNS, TLS and routing work, so `--doctor` distinguishes "missing key" from "network blocked".

## Pricing

Cost columns use a built-in table that can drift as providers change prices. Override per model (repeatable, USD per million tokens):

```bash
llmrace openai --price openai/gpt-4o-mini=1.25:5
```

## Tracking providers over time

Record every race to a local history file (JSONL under `~/.local/share/llmrace/`):

```bash
llmrace openai groq --record
```

Then analyze drift — TTFB/throughput changes versus the rolling median, with model-swap annotations:

```bash
llmrace --trend              # last 30 days, all providers
llmrace --trend --days 7 groq
```

## Using llmrace as a CI quality gate

Record a baseline once, then fail your pipeline when a provider regresses:

```bash
# 1. Record the baseline (commit llmrace-report.json to your repo)
llmrace openai groq --json --out llmrace-report.json

# 2. In CI: race again and compare — exits 1 on regression
llmrace openai groq --baseline llmrace-report.json --max-regression 15
```

The check compares mean latency, p95, TTFB (increases) and tokens/s (decreases) against the threshold, prints an old → new diff table with per-metric Welch p-values, and flags providers that are new or missing from the baseline. With `--require-significance`, a threshold breach only fails the gate when the difference is statistically significant (p < 0.05 on per-run data) — the flake-resistant mode for CI:

```bash
llmrace openai groq --baseline llmrace-report.json --max-regression 15 --require-significance
```

A ready-to-copy workflow lives in [`docs/example-latency-gate.yml`](docs/example-latency-gate.yml). With `--sweep`, each variant lane compares against that provider's matching variant baseline entry, falling back to a plain (variant-less) entry. A run where **every** provider failed exits non-zero — a green pipeline over a total outage is never silent.

## Real-world results

Measured 2026-11-14 from South America — three OpenAI models head-to-head (`openai=model` lanes) vs local inference (Ollama on Apple M5 Pro / 48 GB), 5 rounds each, warmup excluded:

| Provider | Model | TTFB | Latency (mean ± 95% CI) | Tokens/s | Notes |
|---|---|---:|---:|---:|---|
| openai | `gpt-4o-mini` | 567 ms | 1351 ± 78 ms | **215.3** | best all-rounder |
| openai | `gpt-4.1` | 602 ms | 1323 ± 191 ms | 191.8 | |
| openai | `gpt-5-nano` | 1911 ms | 1922 ± 359 ms | 163.0 | 🧠 1275 hidden reasoning tokens |
| ollama | `llama3.2:1b` | 130 ms | 2989 ± 90 ms | 65.3 | local, ~0 network |

The reasoning story, honestly measured: gpt-5-nano spends **1.9 s thinking** before its first visible token (TTFB includes reasoning — see the 🧠 count), then delivers everything in a burst. Once streaming, the cloud lanes sustain 2.5–3.3× local throughput, while the local lane prefills in 130 ms. Tokens/s for reasoning models is computed over the server-side window, not the burst, so the numbers are comparable. In tool-calling mode (`--tools`), gpt-4o-mini returns valid JSON tool arguments 3/3 rounds at ~600 ms round trip, while local `llama3.2:1b` complies only ~80% of the time — validity rate is a real metric, not noise. Reproduce:

```bash
llmrace openai=gpt-4o-mini openai=gpt-4.1 openai=gpt-5-nano -n 5 --warmup 1 --winner
```

PRs with results from other regions are welcome — record with `--json` and extend this table.

## How it compares

| | llmrace | provider CLIs | load testers (k6/vegeta) | router dashboards |
|---|---|---|---|---|
| Multi-provider streaming race | ✅ 14 + any OpenAI-compatible | one at a time | ✝ hand-written scripts | ❌ passive |
| TTFB / tokens/s / p95 / t-CI | ✅ built-in | varies | ✝ you build it | ✅ but only routed traffic |
| Live terminal race view | ✅ | ❌ | ❌ | ❌ |
| CI regression gate (`--baseline`) | ✅ | ❌ | ✝ heavy scripting | ❌ |
| Longitudinal trends + model-swap detection | ✅ | ❌ | ❌ | partial |
| Cost per run from real token usage | ✅ | ❌ | ❌ | partial |
| Single static binary, no config file | ✅ | ❌ | ❌ | ❌ |
| Fuzzed, bounded parser + typed errors | ✅ | — | — | — |

✝ possible with effort — llmrace exists so you don't spend that effort.
Missing a capability you need? [Open an issue](https://github.com/dfbustosus/llmrace/issues/new/choose).

## Engineering

- `#![deny(missing_docs)]` and `#![deny(clippy::unwrap_used)]` on the library
- Typed errors via `thiserror`; `anyhow` only at the binary edge
- Zero-alignment-allocation SSE parser that survives arbitrary network chunk boundaries (exhaustively tested at every split point)
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
