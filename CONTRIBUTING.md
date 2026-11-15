# Contributing to llmrace

Thanks for considering a contribution!

## Adding a provider

Providers live in `src/provider/spec.rs` (registry, endpoints, defaults) — request building is in `request.rs`, stream decoding in `stream.rs`. To add one:

1. Add a constructor (e.g. `Provider::myprovider()`) with the endpoint, env var name, default model and wire protocol. Any OpenAI-compatible API reuses `Wire::OpenAiCompat`.
2. Push it onto `Provider::registry()`.
3. If it has public per-token prices, add a row to the table in `src/cost.rs`.
4. Add or extend a test. Run `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`.

That's it — usually under 40 lines.

## Bug reports

Include the provider, model, OS, and the output of `llmrace --version` plus the error line from the lane.

## Style

- `cargo fmt` formatting, no unwraps in library paths, errors via `anyhow` at the binary edge.
- Keep the TUI dependency-light: `ratatui` + `crossterm` only.

## Module map

| Path | Responsibility |
|---|---|
| `src/provider/spec.rs` | provider registry: ids, endpoints, defaults, availability |
| `src/provider/request.rs` | wire request building + HTTP dispatch |
| `src/provider/stream.rs` | event types, SSE stream, per-wire decoding, mock |
| `src/bench/metrics.rs` | run metrics, bench config, live progress updates |
| `src/bench/summary.rs` | per-lane aggregation and derived stats |
| `src/bench/engine.rs` | lane tasks, rounds, retries, single-request measurement |
| `src/report.rs` | markdown / JSON / CSV / HTML exports |
| `src/gates.rs` | CI quality gates (TTFB, outage detection) |
| `src/regression.rs` | baseline comparison and A/B evaluation |
| `src/score.rs` | composite winner scoring and podiums |
| `src/merge.rs` | cross-region probe merging |
| `src/main.rs` / `src/modes.rs` | binary: CLI definition + dispatch / standalone modes |
| `src/run/` | race execution: `config`, `collect` (lanes), `race`, `ramp`, `watch`, `reports` (writing + gates) |

## Releasing

Maintainers: see [`docs/release.md`](docs/release.md) — tagging is the only
manual step; binaries, crates.io and the Homebrew tap are automated.
