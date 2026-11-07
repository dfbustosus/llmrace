# Security Policy

## Supported versions

Only the latest released version receives security fixes.

## Reporting a vulnerability

Please report vulnerabilities privately:

1. Open a GitHub security advisory: **Security → Report a vulnerability** on this repository, or
2. Email **davidbustosusta@gmail.com** with details.

Please do not open public issues for suspected vulnerabilities. We aim to respond within 72 hours and will credit reporters in the advisory unless anonymity is requested.

## Scope

- The llmrace CLI and library code in this repository
- The GitHub Actions workflows (e.g., secret handling, publish pipeline)

Out of scope: vulnerabilities in the LLM providers themselves, or in dependencies without a RustSec advisory (route those to `cargo-deny` output and we will triage).
