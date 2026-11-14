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

# Local race — real Ollama lane vs the instant mock lane, winner podium
Type "llmrace ollama mock -m ollama=llama3.2:1b -n 2 --warmup 0 --winner"
Enter
Sleep 9s
Enter
Sleep 3s

# Environment diagnostics
Type "llmrace --doctor"
Enter
Sleep 4s
