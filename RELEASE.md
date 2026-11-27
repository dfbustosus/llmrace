# Release process (maintainers)

Releases are tagged from `main` and downstream binary, crates.io, PyPI wheel,
Docker (GHCR), and Homebrew publishing are automated. See
[`docs/release.md`](docs/release.md) for the full maintainer guide and version
policy.

## Checklist

1. **Consolidate `CHANGELOG.md`**
   - Move `[Unreleased]` items under `## [X.Y.Z] - YYYY-MM-DD` (Keep a Changelog)
   - Refresh the comparison links at the bottom of the file
   - Verify every expected entry was written with `grep`
2. **Bump the version & synchronize lockfiles**
   - Update `version` in `Cargo.toml`
   - Run `cargo check` to refresh `Cargo.lock`
   - Run `cargo update --manifest-path fuzz/Cargo.toml --offline -p llmrace` to
     sync `fuzz/Cargo.lock` (avoids a post-release follow-up commit)
   - Confirm `Cargo.toml`, `Cargo.lock`, and `fuzz/Cargo.lock` all match `X.Y.Z`
3. **Verify `main` is green**
   - Rust & docs gate:
     - `cargo fmt --all -- --check`
     - `cargo clippy --locked --all-targets -- -D warnings`
     - `cargo clippy --locked --all-targets --all-features -- -D warnings`
     - `cargo test --locked --all-targets --all-features`
     - `cargo test --locked --doc --all-features`
     - `RUSTDOCFLAGS="-D warnings" cargo doc --locked --all-features --no-deps`
   - Schema & offline consumer contracts (requires `jsonschema==4.25.1`):
     - `cargo run --quiet -- --schema | diff -u docs/schema.json -`
     - `cargo build --locked && python3 tests/contracts.py`
   - Packaging audit:
     - `cargo package --list --allow-dirty` confirms `.agents/**`, `AGENTS.md`,
       `.github/**`, `python/**`, media (`docs/demo*`, `docs/social-preview.png`),
       and `llmrace-report.*` are excluded
   - CI, dual MSRV (**Rust 1.88** default / **Rust 1.89** `--features sqlite`),
     Coverage, Supply chain (`cargo-deny`), and Semver workflows pass
4. **Tag and push**
   ```bash
   git tag vX.Y.Z
   git push origin main
   git push origin vX.Y.Z
   ```
5. **Automation (`.github/workflows/release.yml`, `.github/workflows/docker.yml`, Homebrew tap)**
   - **GitHub Release**: builds seven targets (Linux gnu/musl × x86_64/aarch64,
     macOS ARM/Intel, Windows x86_64) with SHA-256 checksums
   - **crates.io**: runs `cargo publish --locked --dry-run` and publishes
     (skipping cleanly if `vX.Y.Z` is already published)
   - **PyPI wheels**: `pypi` job in `release.yml` verifies archive checksums,
     builds platform wheels via `python/make_wheel.py`, and uploads with
     `PYPI_API_TOKEN` (`publish-pypi.yml` handles manual backfills)
   - **Docker (GHCR)**: `docker.yml` triggers after `Release` succeeds, builds
     and scans multi-arch `ghcr.io/dfbustosus/llmrace` and
     `ghcr.io/dfbustosus/llmrace-scratch` images, and tags the release
   - **Homebrew**: `dfbustosus/homebrew-tap` (`Bump formula` workflow) updates
     `Formula/llmrace.rb` on its hourly schedule or via `gh workflow run`

## Failure handling

- **Binary target or workflow failure**: fix on `main` and cut the next patch
  version; never move or delete a published tag.
- **Token expired**: rotate the relevant secret (`CARGO_REGISTRY_TOKEN`,
  `PYPI_API_TOKEN`) in repository secrets.
