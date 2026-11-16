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

# Multi-model race — classic vs reasoning vs local, with significance verdict
Type "llmrace openai=gpt-4o-mini openai=gpt-5-nano ollama=llama3.2:1b mock -n 3 --warmup 0 --winner"
Enter
Sleep 12s
Enter
Sleep 3s

# Live model discovery + diagnostics
Type "llmrace --models ollama"
Enter
Sleep 2s
Type "llmrace --doctor"
Enter
Sleep 5s
