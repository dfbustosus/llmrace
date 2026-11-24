Output docs/demo.gif
Require llmrace
Set FontSize 14
Set Width 1280
Set Height 640
Set Theme "Catppuccin Mocha"
Set WindowBar Rings
Set Padding 18
Set TypingSpeed 10ms
Set CursorBlink true

# Four real models race one workload: OpenAI direct, three via OpenRouter.
# Every lane passes; the verdict says the gap could still be noise.
Type "llmrace openai=gpt-4o-mini openrouter=anthropic/claude-haiku-4.5 openrouter=deepseek/deepseek-chat-v3.1 openrouter=meta-llama/llama-3.3-70b-instruct --no-tui -n 3 --warmup 1 --workload examples/workloads/demo-local.jsonl --winner"
Enter
Sleep 18s
