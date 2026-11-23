# Architecture

llmrace is a lib + bin crate: everything except CLI argument parsing and
terminal presentation lives in the library so the benchmark engine can be
embedded.

## Module map

```
main.rs            CLI (clap), profile/provider resolution, exit codes
├── modes          standalone CLI modes (--init, --doctor, --compare, --merge, --suite)
├── run/           multi-lane race, --ramp saturation curve, --watch loop, report I/O
└── lib.rs
    ├── config     llmrace.toml profiles (defaults < profile < CLI)
    ├── variants   built-in and custom prompt shapes for --sweep
    ├── workload   shared JSONL prompt workloads (exact / contains / regex)
    ├── tools      tool-call tasks, schemas, and answer validation
    ├── provider   registry + wire protocols + SigV4 / OAuth / streaming plumbing
    ├── sse        incremental SSE byte-stream parser (bounded 1 MiB buffer)
    ├── bench      lanes, rounds, retries, concurrency, RunMetrics / LaneSummary
    ├── stats      Sample: percentiles (linear interp), t-CI, Welch t-test, 3σ outliers
    ├── cost       per-model pricing, cache discounts, and rate overrides
    ├── score      weighted composite podium scoring (TTFB, tok/s, p95, task, cost)
    ├── gates      all-lanes-failed and --max-ttfb CI gates
    ├── regression --baseline comparison, request_sha256 guard, per-prompt gate
    ├── report     markdown / JSON / CSV / HTML renderers and request hashing
    ├── schema     JSON Schema generator for race.json (--schema)
    ├── suite      multi-race benchmark suite definitions and combined report
    ├── merge      cross-region distributed probe report merging
    ├── history    JSONL persistence + --trend drift analysis
    ├── sqlite     optional SQLite history backend (feature `sqlite`)
    ├── recommend  historical provider recommendation
    ├── dashboard  self-contained static HTML trend dashboard
    ├── prom       Prometheus textfile / Pushgateway exporter
    ├── notify     Slack/Discord alert webhooks and desktop notifications
    ├── doctor     environment, endpoint egress, and local daemon diagnostics
    └── tui        live race view (ratatui)
```

## Data flow

```
CLI → resolve providers (+ profile, workload, sweep, custom endpoints)
    → spawn_lane_stoppable(spec) per (provider × variant)   [tokio tasks]
        round: JoinSet of `concurrency` single_request()
            → stream_completion() → wire codec → StreamEvent
            → progress via mpsc (TUI) + RunMetrics
    → LaneSummary (aggregates via Sample, goodput, per-prompt PromptRow)
    → composite scoring (--winner) → reports (md/json/csv/html)
    → history (--record) → gates (all_lanes_failed, --max-ttfb, --baseline)
```

## Concurrency model

- **Lanes run concurrently** across providers so they experience the same
  network conditions; **rounds are sequential within a lane** so a provider
  never throttles itself mid-measurement (`provider=model` lanes of the same
  provider race concurrently to measure shared rate-limit behavior).
- Each request runs in a `JoinSet` task; join errors and panics become lane
  errors — nothing escapes to abort the process.
- Per-request timeouts are enforced mid-stream with `tokio::select!`.
- Progress flows over a plain `std::sync::mpsc` to the TUI thread
  (`spawn_blocking`), decoupled from the async runtime.

## Wire protocols

`Wire` selects the request/response codec:

| Wire | Used by | Streaming shape |
|---|---|---|
| `OpenAiCompat` | OpenAI, Groq, OpenRouter, Ollama, DeepSeek, Mistral, Together, Fireworks, Cerebras, DeepInfra, Hyperbolic, xAI, Perplexity, LM Studio, llama.cpp, Azure, custom | `choices[].delta.content` + `usage` |
| `Anthropic` | Anthropic | `content_block_delta` + `message_start` / `message_delta` usage |
| `Gemini` | Google Gemini | `candidates[].content.parts[].text` + `usageMetadata` |
| `Cohere` | Cohere v2 | `content-delta` + `message-end` billed units |
| `Bedrock` | Amazon Bedrock Converse Stream (experimental) | SigV4 + binary event-stream (`contentBlockDelta`, `metadata`) |
| `Vertex` | Google Vertex AI (experimental) | Service-account RS256 JWT OAuth2 + Gemini SSE wire |
| `Mock` | tests and no-key demos | deterministic 24-token stream |

Adding an OpenAI-compatible provider is a constructor + registry entry (~40
lines); adding a new protocol is a `Wire` variant plus request/stream arms.

## Measurement methodology

- **TTFT / TTFB**: from immediately before the request is sent to the first
  streamed output event — includes connect, TLS, upload, queueing, and
  reasoning pre-fill. Usage-only events do not start the TTFT clock.
- **Effective generation window**: `max(total − TTFB, server_ms − TTFB)` so
  post-thinking token bursts cannot inflate throughput.
- **Goodput ranking**: tables rank lanes by `tokens/s × success_rate` so a
  provider dropping rounds cannot win on survivors alone.
- **Saturation mode** (`--concurrency N`): a round aggregates all N requests —
  wall-clock is `max(total_ms)`, tokens are summed — measuring aggregate
  throughput.
- **Statistics**: percentiles use linear interpolation between closest ranks;
  95% CI uses Student-t critical values; Welch's t-test checks whether gaps and
  regressions are distinguishable from noise (`p < 0.05`); 3σ rounds are
  flagged (`†`).
- **Request identity**: `request_sha256` hashes prompt/workload, `expect` and
  `match` mode, `max_tokens`, sampling, system prompt, and tool task so
  `--baseline` refuses comparisons when the benchmarked request changed.

## Untrusted-input hardening

- The SSE parser buffers at most 1 MiB without a separator, then errors with
  `Error::Protocol`; workload answer regexes cap compiled DFA size at 1 MiB.
- Parsers are fuzzed (`fuzz/`, scheduled CI bursts) and exhaustively unit
  tested across chunk split boundaries.
- `#![forbid(unsafe_code)]` and `#![deny(clippy::unwrap_used)]` across the
  library: zero unsafe blocks and zero panicking unwraps on runtime paths.

## Release pipeline

Tag `vX.Y.Z` → `.github/workflows/release.yml` builds platform archives with
SHA-256 checksums → creates the GitHub Release → publishes to crates.io (or
skips cleanly when that version was already published). An hourly workflow in
`dfbustosus/homebrew-tap` updates `Formula/llmrace.rb` automatically from the
latest GitHub Release.
