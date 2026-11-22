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

# No API key. Two lanes, the significance line, and the podium.
Type "llmrace mock mock --no-tui -n 3 --warmup 0 --winner"
Enter
Sleep 6s
