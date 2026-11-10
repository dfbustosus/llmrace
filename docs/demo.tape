Output docs/demo.gif
Set FontSize 15
Set Width 1100
Set Height 520
Set Theme "Dark+"
Set Padding 14
Set TypingSpeed 40ms
Set CursorBlink true

Type@40ms "llmrace openai ollama mock -m ollama=llama3.2:1b -n 1 --warmup 0"
Enter
Sleep 7s
Type "q"
Sleep 800ms
Type "llmrace --doctor --json | head -20"
Enter
Sleep 3s
