# Launch kit

Everything needed to put llmrace in front of people. Post order: Show HN
(Tuesday–Thursday, ~7–9am PT), then Reddit the same day, then let the
GitHub Action / Docker discoverability do long-tail work.

The pitch has two halves, and the second one is what no other LLM
benchmark does: (1) race providers honestly, and (2) keep them honest
forever — llmrace is a CI gate, and the failing prompt shows up as a
failing test.

## Show HN

**Title:** Show HN: LLM benchmark that refuses to lie to you (and gates CI on your prompts)

**Text:**

I kept seeing teams pick an LLM provider off a single benchmark run —
three requests, a mean latency, done — and then ship prompts that a
silent provider update degrades weeks later. So I built llmrace, a
single-binary CLI that races providers head-to-head and then stays in
your pipeline:

- **A CI gate that names the prompt.** Record a baseline of your workload
  once; `--baseline` exits 1 when one prompt's success falls or its
  latency rises past the threshold — even when the lane average is fine.
  Welch's test on that prompt's rounds decides what is noise
  (`--require-significance`), and if the request itself changed, the gate
  refuses to compare at all. `race.json` lists
  `regression.failures[].prompt_id`.
- **It speaks CI natively.** `--junit` turns the failing prompt into a
  failing test on the pull request. `--sarif` turns every gate failure
  into a code-scanning alert. In Rust, skip the binary:
  `Race::new([...]).run().await?.gate(&baseline, GatePolicy::ci_default())`
  inside a `#[tokio::test]`.
- **Statistical significance, not vibes.** Welch's t-test between the top
  two providers, on per-run latencies. One run told me gpt-4o-mini vs
  gpt-5-nano was "Δ −351 ms, significant (p = 0.010)". Another told me a
  47 ms gap was "NOT significant (p = 0.85) — the gap could be noise."
  Most leaderboards would have ranked them anyway.
- **Reasoning models measured honestly.** gpt-5/o-series think before the
  first visible token, then dump everything at once. Naive tools/s report
  30,000 tok/s for gpt-5-nano. llmrace splits hidden reasoning tokens
  (🧠 count) from visible ones and computes throughput over the honest
  window: ~176 tok/s.
- **Real workloads, not toy prompts.** A workload line can be a
  multi-turn conversation with its own system prompt, demand the answer
  match a JSON Schema, or demand a tool call whose arguments match a
  schema you supply — on six provider wire protocols. Warmup can't
  secretly prime the KV cache (`--warmup-mode probe`), and the report
  says when cold and warm cache rounds got mixed into one latency sample.

Also in the box: live TUI race view, TTFT/inter-token-latency/cost per
provider (cache discounts priced), saturation curves (`--ramp 1,2,4,8`),
cross-region probe merging, watch mode with debounced alerts, the OpenAI
Responses API as a first-class wire next to Chat Completions, one-line
GitHub Action, Homebrew/Docker/crates.io distribution. 100% safe Rust,
no telemetry, no config file required.

```bash
brew install dfbustosus/tap/llmrace
llmrace mock mock --no-tui -n 3 --warmup 0 --winner   # no key needed
llmrace openai anthropic groq gemini --winner
```

Happy to talk methodology — the honesty features each came from a real
measurement bug (the changelog keeps the receipts).

## r/LocalLLaMA / r/MachineLearning variant

**Title:** I built a CLI that races LLM providers with error bars (and
catches when your "benchmark" is just noise)

Lead with the Ollama angle for r/LocalLLaMA: race your local models
against cloud APIs from your terminal, see thinking-time vs streaming
time split, ITL jitter, saturation curves for concurrency tuning. Then
the eval-harness angle for r/MachineLearning: the same workload file
drives a CI gate — prompt pass/fail as JUnit tests, regressions as SARIF
alerts, per-prompt latency with Welch significance — so an eval stops
being a spreadsheet someone reruns by hand.

## r/rust variant

**Title:** llmrace — a TUI benchmarking CLI in 100% safe Rust (forbid(unsafe)),
fuzzed parsers, no-unwrap binary, semver-checked releases

Focus: ratatui live view, custom SSE parser O(n) + fuzz targets, Welch
stats without dependencies (incomplete beta + Lanczos, textbook-verified),
automated releases from tag to Homebrew. It's also a library:
`llmrace::race::Race` runs a full multi-lane race from your own test
suite with scoped per-race pricing (no global state), and the CLI's
JUnit/SARIF/JSON renderers are public functions.

## Checklist

- [ ] Post Show HN; reply to every comment in the first 2 hours
- [ ] Same-day Reddit posts (tailor per subreddit above)
- [ ] Pin a GitHub discussion for feedback
- [ ] Verify demo GIF renders on each platform (HN strips images —
      link it: raw GitHub URL of docs/demo.gif)
- [x] Have one real fresh race ready to paste: the 2026-11-24
      four-model table in the README (OpenAI direct + three via
      OpenRouter, Pass 3/3, cost on every lane, p = 0.003 verdict).
      Gist: https://gist.github.com/dfbustosus/3400b6fe9396cea972dc496a6105bcca
- [ ] Have the CI workflow ready to paste too:
      docs/example-latency-gate.yml (baseline → gate → JUnit + SARIF)
- [ ] Star-gazing is fine; watch for issue #126/#127 (Bedrock/Vertex
      good-first-issues) attracting contributors from the traffic
