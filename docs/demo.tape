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

# A real local model answers three prompts — and gets one genuinely wrong.
Type "llmrace ollama ollama -m llama3.2:1b --no-tui -n 3 --warmup 1 --workload examples/workloads/demo-local.jsonl --reproducible --winner"
Enter
Sleep 14s
