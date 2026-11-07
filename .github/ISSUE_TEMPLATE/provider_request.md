---
name: Provider request
about: Ask for a new LLM provider to be added to the race
title: "[provider] "
labels: enhancement, good first issue
assignees: ""
---

**Provider**
Name and link to the API documentation.

**Wire protocol**
- [ ] OpenAI-compatible (`/v1/chat/completions`) — cheapest to add, see `ProviderSpec::custom`
- [ ] Native protocol (describe the streaming format below)

**Details needed**
- Endpoint URL:
- Auth: env var name / header format:
- Default model to race:
- Public pricing (USD per million tokens, input/output):

**Anything else**
Rate limits, regions, quirks.

Adding a provider is usually ~40 lines — see CONTRIBUTING.md; feel free to open the PR yourself.
