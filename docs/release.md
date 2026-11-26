# Release guide (maintainers)

Releases are tagged from `main` and everything downstream is automated.

## Before tagging

1. **Changelog**: consolidate `[Unreleased]` into a `## [X.Y.Z] - YYYY-MM-DD`
   section (Keep a Changelog). Every entry must be present in the written
   file — the repo's history includes incidents of silent no-op edits, so
   verify with `grep` after writing.
2. **Version**: bump `version` in `Cargo.toml` (`cargo check` refreshes the
   lockfile).
3. **Green main**: all verification workflows must pass
   (CI, MSRV, Coverage, Supply chain, Semver, plus the fuzz burst on
   schedule). The Semver job enforces the stability policy — additive
   struct fields warn (`constructible_struct_adds_field`), removals and
   signature changes fail.
4. **Schema**: `docs/schema.json` must match `cargo run -- --schema`
   (CI checks this on every push). Real reports must also pass
   `python tests/contracts.py`; matching schema text alone is insufficient.
5. **Library additions**: new fields on public structs require a minor
   release under this project's compatibility policy. Announce those fields
   in the changelog so downstream exhaustive literals can be updated.

## Tagging

```bash
git tag vX.Y.Z
git push origin vX.Y.Z
```

The `release` workflow then, in order:
1. Checks that the tag matches `Cargo.toml`, then builds seven targets
   (Linux GNU/musl × x86_64/aarch64, macOS ARM + Intel, Windows x86_64)
   with the lockfile and SHA-256 checksums.
2. Creates the GitHub release.
3. Verifies the version is not already on crates.io, then publishes.

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
