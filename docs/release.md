# Release guide (maintainers)

Releases are tagged from `main` and everything downstream is automated.

## Before tagging

1. **Changelog**: consolidate `[Unreleased]` into a `## [X.Y.Z] - YYYY-MM-DD`
   section (Keep a Changelog) and refresh the comparison links at the bottom of
   `CHANGELOG.md`. Every entry must be present in the written file — the repo's
   history includes incidents of silent no-op edits, so verify with `grep` after
   writing.
2. **Version & lockfiles**: bump `version` in `Cargo.toml`, refresh `Cargo.lock`
   (`cargo check`), and synchronize `fuzz/Cargo.lock` so the fuzz workspace
   records the new `llmrace` version without a follow-up commit:
   ```bash
   cargo check
   cargo update --manifest-path fuzz/Cargo.toml --offline -p llmrace
   ```
   Verify `Cargo.toml`, `Cargo.lock`, and `fuzz/Cargo.lock` all report `X.Y.Z`.
3. **Local verification gate**: run the full deterministic check matrix before
   tagging:
   ```bash
   cargo fmt --all -- --check
   cargo clippy --locked --all-targets -- -D warnings
   cargo clippy --locked --all-targets --all-features -- -D warnings
   cargo test --locked --all-targets --all-features
   cargo test --locked --doc --all-features
   RUSTDOCFLAGS="-D warnings" cargo doc --locked --all-features --no-deps
   ```
4. **Schema & consumer contracts**: `docs/schema.json` must match
   `cargo run --quiet -- --schema` byte-for-byte (`diff -u docs/schema.json -`;
   CI checks this on every push). Real reports, strict gates, PTY TUI, watch
   persistence, and wheel builds must also pass the offline contract suite
   (requires `jsonschema==4.25.1`):
   ```bash
   cargo build --locked && python3 tests/contracts.py
   ```
   Matching schema text alone is insufficient.
5. **Packaging audit**: verify the crates.io tarball excludes repo-only files
   and heavy media (`.agents/**`, `AGENTS.md`, `.github/**`, `python/**`,
   `docs/demo*.gif`, `docs/demo*.tape`, `docs/social-preview.png`, and
   `llmrace-report.*`):
   ```bash
   cargo package --list --allow-dirty
   ```
6. **Green main & dual MSRV**: all verification workflows must pass (CI, MSRV,
   Coverage, Supply chain `cargo-deny`, Semver, plus the fuzz burst on
   schedule). Default features must compile on **Rust 1.88** and `--features sqlite`
   must compile on **Rust 1.89**. The Semver job enforces the stability policy —
   additive struct fields warn (`constructible_struct_adds_field`), removals and
   signature changes fail.
7. **Library additions**: new fields on public structs require a minor release
   under this project's compatibility policy. Announce those fields in the
   changelog so downstream exhaustive literals can be updated.

## Tagging

```bash
git tag vX.Y.Z
git push origin main
git push origin vX.Y.Z
```

The `release` workflow (`.github/workflows/release.yml`) then, in order:
1. Checks that the tag matches `Cargo.toml`, then builds seven targets
   (Linux GNU/musl × x86_64/aarch64, macOS ARM + Intel, Windows x86_64)
   with `--locked` and SHA-256 checksums.
2. Creates the GitHub release.
3. Verifies the version is not already on crates.io, runs
   `cargo publish --locked --dry-run`, then publishes.
4. Builds and uploads platform wheels to PyPI (when `PYPI_API_TOKEN` is set).

## After the release

The **Homebrew tap updates itself**: an hourly workflow in
`dfbustosus/homebrew-tap` resolves the latest release, rewrites version
strings (URL path, artifact filenames, `version` field — all three) and
checksums in `Formula/llmrace.rb`, and commits. Verify with:

```bash
gh run list --repo dfbustosus/homebrew-tap --workflow bump --limit 1
brew upgrade llmrace && llmrace --version
```

The **PyPI wheels build in the `pypi` job of `release.yml`** after the
GitHub release exists. The packager verifies archive checksums and includes
both license texts. With a `PYPI_API_TOKEN` repository secret the job
uploads the wheels to PyPI. `publish-pypi.yml` is the manual backfill workflow:
```bash
gh workflow run publish-pypi.yml -f tag=vX.Y.Z
```

Docker resolves the originating release version once and uses it for both
image builds and manifest tags. Archives are checksum-verified, binaries
must report the requested version, and images are scanned before version
tags are published. Backfills publish the requested version tag; only the
latest release updates rolling aliases. If a release fails verification,
fix forward with a new patch/minor release; never replace a published tag.

Prebuilt binaries currently use default Cargo features (MSRV 1.88). SQLite
users need Rust 1.89 and build with `cargo install llmrace --features sqlite`; `--version-json` reports
`sqlite_enabled` so installers and automation can check availability.

## Version policy

- Patch (x.y.Z): fixes only.
- Minor (x.Y.0): additive features; report schemas gain fields
  additively; struct fields may be added (documented policy).
- Major (X.0.0): breaking library changes only.

MSRV bumps are minor-release changes, announced in the changelog.
