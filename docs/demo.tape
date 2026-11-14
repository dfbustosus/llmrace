Output docs/demo.gif
Require llmrace

Set FontSize 16
Set Width 1000
Set Height 560
Set Theme "Catppuccin Mocha"
Set WindowBar Rings
Set Padding 18
Set TypingSpeed 10ms
Set CursorBlink true

# Multi-model race — two OpenAI models head-to-head vs local Ollama vs the instant mock
Type "llmrace openai=gpt-4o-mini openai=gpt-4.1 ollama=llama3.2:1b mock -n 2 --warmup 0 --winner"
Enter
Sleep 9s
Enter
Sleep 3s

# Environment diagnostics
Type "llmrace --doctor"
Enter
Sleep 4s
