# Architecture

llmrace is a lib + bin crate: everything except argument parsing lives in the
library so the benchmark engine can be embedded.

## Module map

```
main.rs            CLI (clap), wiring, exit codes
├── config         llmrace.toml profiles (defaults < profile < CLI)
├── variants       built-in prompt shapes for --sweep
├── provider       registry + wire protocols + streaming plumbing
│     └── sse      incremental SSE byte-stream parser (bounded buffer)
├── bench          lanes, rounds, concurrency, RunMetrics/LaneSummary
│     └── stats    Sample: percentiles (linear interp), t-CI, 3σ outliers
├── regression     --baseline comparison for CI gates
├── history        JSONL persistence + --trend analysis
├── report         markdown / JSON / CSV renderers
├── watch          interval mode with rolling-median alerts
└── tui            live race view (ratatui)
```

## Data flow

```
CLI → resolve providers (+ profile, sweep, custom endpoints)
    → spawn_lane(spec) per (provider × variant)          [tokio tasks]
        round: JoinSet of `concurrency` single_request()
            → stream_completion() → HTTP SSE → SseParser → decode_payload
            → progress via mpsc (TUI) + RunMetrics
    → LaneSummary (aggregates via Sample)
    → reports (md/json/csv) → history (--record) → regression check (--baseline)
```

## Concurrency model

- **Lanes run concurrently** across providers so they experience the same
  network conditions; **rounds are sequential within a lane** so a provider
  never throttles itself mid-measurement.
- Each request runs in a `JoinSet` task; join errors and panics become lane
  errors — nothing escapes to abort the process.
- Per-request timeouts are enforced mid-stream with `tokio::select!`.
- Progress flows over a plain `std::sync::mpsc` to the TUI thread
  (`spawn_blocking`), decoupled from the async runtime.

## Wire protocols

`Wire` selects the request/response codec:

| Wire | Used by | Streaming shape |
|---|---|---|
| `OpenAiCompat` | OpenAI, Groq, OpenRouter, Ollama, DeepSeek, Mistral, Together, Fireworks, Cerebras, custom | `choices[].delta.content` + `usage.completion_tokens` |
| `Anthropic` | Anthropic | `content_block_delta` + `message_delta.usage` |
| `Gemini` | Google | `candidates[].content.parts[].text` + `usageMetadata` |
| `Mock` | tests/demos | deterministic 24-token stream |

Adding a provider is a constructor + registry entry (~40 lines); adding a
*protocol* is a new `Wire` variant plus `decode_payload` arm and a request
builder branch.

## Measurement methodology

- **TTFB**: from immediately before the request is sent to the first
  streamed event — includes connect, TLS, upload and server queueing.
- **Generation window**: total − TTFB; tokens/s divides provider-reported
  completion tokens by this window.
- **Saturation mode** (`--concurrency N`): a round aggregates all N requests —
  wall-clock is `max(total_ms)`, tokens are summed — so tokens/s measures
  aggregate throughput, not per-stream averages.
- **Statistics**: percentiles use linear interpolation between closest ranks;
  the 95% CI on the mean uses a Student-t critical-value table
  (interpolated, `z=1.96` beyond df=30); rounds beyond 3σ from the mean are
  flagged as outliers (`†`).
- Warmup rounds (default 1) absorb TLS/conn setup and are excluded.

## Untrusted-input hardening

- The SSE parser buffers at most 1 MiB without a separator, then errors with
  `Error::Protocol` — no unbounded growth from hostile streams.
- The parser is fuzzed (`fuzz/`, weekly CI bursts) and exhaustively unit
  tested at every possible chunk split boundary.
- `#![deny(clippy::unwrap_used)]` in the library: no panics on the data path.

## Release pipeline

Tag `vX.Y.Z` → CI builds 4 platform binaries with checksums → GitHub Release
→ `cargo publish` to crates.io using the `CARGO_REGISTRY_TOKEN` secret (with a
version-already-published guard). Homebrew formula lives in
`dfbustosus/homebrew-tap`.
