# Release process (maintainers)

Releases are tagged from `main` and downstream binary, crates.io, and Homebrew
publishing are automated. See [`docs/release.md`](docs/release.md) for the full
maintainer guide.

## Checklist

1. **Consolidate `CHANGELOG.md`**
   - Move `[Unreleased]` items under `## [X.Y.Z] - YYYY-MM-DD` (Keep a Changelog)
   - Refresh the comparison links at the bottom of the file
2. **Bump the version**
   - Update `version` in `Cargo.toml`
   - Run `cargo check` so `Cargo.lock` records the new version
3. **Verify `main` is green**
   - `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings`,
     `cargo test --all-targets`, and `cargo test --doc`
   - `cargo run -- --schema` matches `docs/schema.json`
   - CI, MSRV, Coverage, Supply chain (`cargo-deny`), and Semver workflows pass
4. **Tag and push**
   ```bash
   git tag vX.Y.Z
   git push origin main
   git push origin vX.Y.Z
   ```
5. **Automation (`.github/workflows/release.yml` + Homebrew tap)**
   - Builds Linux (gnu/musl, x86_64/aarch64), macOS (ARM/Intel), and Windows
     archives with SHA-256 checksums
   - Creates the GitHub Release
   - Publishes to crates.io (skipping cleanly if `vX.Y.Z` is already published)
   - `dfbustosus/homebrew-tap` (`Bump formula` workflow) updates
     `Formula/llmrace.rb` on its hourly schedule or via `gh workflow run`

## Failure handling

- **Binary target or workflow failure**: fix on `main` and cut the next patch
  version; never move or delete a published tag.
- **Token expired**: rotate the crates.io token and update
  `CARGO_REGISTRY_TOKEN` in repository secrets.
