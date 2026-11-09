Output docs/demo.gif
Set FontSize 16
Set Width 1000
Set Height 460
Set Theme "Dark+"
Set Padding 16
Set TypingSpeed 50ms
Set CursorBlink true

Type@40ms "llmrace ollama mock -m llama3.2:1b --sweep short,json -n 1 --warmup 0"
Enter
Sleep 5s
Type "q"
Sleep 600ms
Type "llmrace --doctor"
Enter
Sleep 3s
