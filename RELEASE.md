# Release process (maintainers)

Releases are almost entirely automated. The manual steps are: bump, document, tag.

## Checklist

1. **Bump the version**
   - `Cargo.toml` → `version = "X.Y.Z"`
   - Run `cargo check` so `Cargo.lock` picks up the new version
2. **Update `CHANGELOG.md`**
   - Move `[Unreleased]` items under a new `## [X.Y.Z] - YYYY-MM-DD` heading
   - Refresh the compare links at the bottom
3. **Verify CI is green on `main`** (tests, MSRV, cargo-deny)
4. **Tag and push**
   ```bash
   git tag -a vX.Y.Z -m "llmrace vX.Y.Z"
   git push origin main --tags
   ```
5. **Automation takes over** (`.github/workflows/release.yml`):
   - Builds release binaries: Linux x86_64, macOS ARM + Intel, Windows x86_64
   - Packages tarballs/zips with SHA-256 checksums
   - Creates the GitHub Release with generated notes
   - Publishes the crate to crates.io using the `CARGO_REGISTRY_TOKEN` secret
     (a version guard fails the job early if X.Y.Z already exists on crates.io)
6. **Post-release verification**
   - `cargo install llmrace` from a clean machine
   - `cargo binstall llmrace` fetches the new version
   - docs.rs rebuilt the documentation
   - Update the `brew` formula in `dfbustosus/homebrew-tap` (until automated)

## Failure handling

- **Publish step failed with "already published"**: you forgot to bump; bump and re-tag as a patch.
- **Token expired**: create a new crates.io token (publish-new scope) and update the `CARGO_REGISTRY_TOKEN` repo secret.
- **A binary target broke**: fix in a PR, then create a new tag moving the patch version forward; never move an existing tag.
