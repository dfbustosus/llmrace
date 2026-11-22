# Launch kit

Everything needed to put llmrace in front of people. Post order: Show HN
(Tuesday–Thursday, ~7–9am PT), then Reddit the same day, then let the
GitHub Action / Docker discoverability do long-tail work.

## Show HN

**Title:** Show HN: LLM benchmark that refuses to lie to you

**Text:**

I kept seeing teams pick an LLM provider off a single benchmark run —
three requests, a mean latency, done. So I built llmrace, a single-binary
CLI that races providers head-to-head and reports what most tools hide:

- **Statistical significance.** Welch's t-test between the top two
  providers, on per-run latencies. Yesterday's run told me gpt-4o-mini vs
  gpt-5-nano was "Δ −351 ms, significant (p = 0.010)". Today's told me a
  gap of 47 ms was "NOT significant (p = 0.85) — the gap could be noise."
  Most leaderboards would have ranked them anyway.
- **Reasoning models measured honestly.** gpt-5/o-series think before the
  first visible token, then dump everything at once. Naive tools/s report
  30,000 tokens/s for gpt-5-nano. llmrace splits hidden reasoning tokens
  (🧠 count) from visible ones and computes throughput over the honest
  window: ~176 tok/s.
- **A CI gate that names the prompt.** `--baseline` exits 1 when one
  prompt's success falls, or when the request changed. The JSON report
  lists `regression.failures[].prompt_id`. `--require-significance`
  treats a noisy latency gap as noise and passes.

Also in the box: live TUI race view, TTFB/inter-token-latency/cost per
provider, saturation curves (`--ramp 1,2,4,8`), cross-region merging of
probe artifacts, watch mode with debounced alerts, custom prompt sweeps,
JSON Schema'd reports, one-line GitHub Action, Homebrew/Docker/crates.io
distribution. 100% safe Rust, no telemetry, no config file required.

```bash
brew install dfbustosus/tap/llmrace
llmrace mock mock --no-tui -n 3 --warmup 0 --winner   # no key
llmrace openai anthropic groq gemini --winner
```

Happy to talk methodology — the honesty features each came from a real
measurement bug (the README's changelog keeps the receipts).

## r/LocalLLaMA / r/MachineLearning variant

**Title:** I built a CLI that races LLM providers with error bars (and
catches when your "benchmark" is just noise)

Lead with the Ollama angle for r/LocalLLaMA: race your local models
against cloud APIs from your terminal, see thinking-time vs streaming
time split, ITL jitter graphs, saturation curves for concurrency tuning.

## r/rust variant

**Title:** llmrace — a TUI benchmarking CLI in 100% safe Rust (forbid(unsafe)),
fuzzed parsers, no-unwrap binary, semver-checked releases

Focus: ratatui live view, custom SSE parser O(n) + fuzz targets, Welch
stats without dependencies (incomplete beta + Lanczos, textbook-verified),
automated releases from tag to Homebrew.

## Checklist

- [ ] Post Show HN; reply to every comment in the first 2 hours
- [ ] Same-day Reddit posts (tailor per subreddit above)
- [ ] Pin a GitHub discussion for feedback
- [ ] Verify demo GIF renders on each platform (HN strips images —
      link it: raw GitHub URL of docs/demo.gif)
- [ ] Have one real fresh race ready to paste (run with --json, upload
      the .md as a gist) — nothing sells it like live numbers
- [ ] Star-gazing is fine; watch for issue #126/#127 (Bedrock/Vertex
      good-first-issues) attracting contributors from the traffic
