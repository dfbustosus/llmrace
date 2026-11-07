## Summary

What does this PR change and why?

## Checklist

- [ ] `cargo fmt` clean
- [ ] `cargo clippy --all-targets -- -D warnings` clean
- [ ] `cargo test --all-targets` green
- [ ] Tests added or updated for the behavior change
- [ ] Rustdoc on new public items (`#![deny(missing_docs)]` enforced)
- [ ] No `unwrap()` in library paths (`#![deny(clippy::unwrap_used)]` enforced)
- [ ] CHANGELOG.md updated under `[Unreleased]` (for user-facing changes)
- [ ] README updated (new flags, providers, reports)

## Related issues

Closes #N (if applicable)
