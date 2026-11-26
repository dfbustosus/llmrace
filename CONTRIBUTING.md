# Contributing to llmrace

Thanks for considering a contribution!

## Adding a provider

Providers live in `src/provider/spec.rs` (registry, endpoints, defaults) — request building is in `request.rs`, stream decoding in `stream.rs`. To add one:

1. Add a constructor (e.g. `ProviderSpec::myprovider()`) with the endpoint, env var name, default model and wire protocol. Any OpenAI-compatible API reuses `Wire::OpenAiCompat`.
2. Push it onto `ProviderSpec::registry()`.
3. If it has public per-token prices, add a row to the table in `src/cost.rs`.
4. Add or extend a test, including an independently sourced completion/error fixture for a new wire. Run the checks below.

That's it — usually under 40 lines.

## Local checks

Use Rust 1.89 or newer and Python 3.11+ for the all-feature checks below. Default features
support Rust 1.88; SQLite's `rusqlite` dependency requires Rust 1.89.

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets --all-features
cargo test --locked --doc --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --locked --all-features --no-deps
cargo build --locked --all-features
# In a Python virtual environment:
python -m pip install jsonschema==4.25.1
python tests/contracts.py
```

The consumer checks validate real reports, strict gate output, single-worker
terminal execution, watch persistence, and offline wheel installation. They
exclude provider credentials. A useful first contribution is one bounded
protocol fixture or failure-path regression test; native cloud authentication
integrations should be split into smaller issues.

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
