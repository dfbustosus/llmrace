# Release guide (maintainers)

Releases are tagged from `main` and everything downstream is automated.

## Before tagging

1. **Changelog**: consolidate `[Unreleased]` into a `## [X.Y.Z] - YYYY-MM-DD`
   section (Keep a Changelog). Every entry must be present in the written
   file — the repo's history includes incidents of silent no-op edits, so
   verify with `grep` after writing.
2. **Version**: bump `version` in `Cargo.toml` (`cargo check` refreshes the
   lockfile).
3. **Green main**: all six workflows must pass
   (CI, MSRV, Coverage, Supply chain, Semver, plus the fuzz burst on
   schedule). The Semver job enforces the stability policy — additive
   struct fields warn (`constructible_struct_adds_field`), removals and
   signature changes fail.
4. **Schema**: `docs/schema.json` must match `cargo run -- --schema`
   (CI checks this on every push).

## Tagging

```bash
git tag vX.Y.Z
git push origin vX.Y.Z
```

The `release` workflow then, in order:
1. Builds four targets (Linux x86_64, macOS ARM + Intel, Windows x86_64)
   with SHA-256 checksums.
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

## Version policy

- Patch (x.y.Z): fixes only.
- Minor (x.Y.0): additive features; report schemas gain fields
  additively; struct fields may be added (documented policy).
- Major (X.0.0): breaking library changes only.

MSRV bumps are minor-release changes, announced in the changelog.
