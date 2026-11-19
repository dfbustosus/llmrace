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

# Multi-model race with significance verdict and winner podium
Type "llmrace openai=gpt-4o-mini ollama=llama3.2:1b mock -n 3 --warmup 0 --winner"
Enter
Sleep 10s
Enter
Sleep 3s

# Provider recommendation from accumulated history
Type "llmrace --recommend"
Enter
Sleep 3s

# Diagnostics
Type "llmrace --doctor"
Enter
Sleep 5s
