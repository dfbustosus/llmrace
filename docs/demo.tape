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

# Four real models stream a paragraph live, side by side.
# q when the race is done — then the verdict and the podium.
Type "llmrace openai=gpt-4o-mini openrouter=anthropic/claude-haiku-4.5 openrouter=deepseek/deepseek-chat-v3.1 openrouter=meta-llama/llama-3.3-70b-instruct -n 3 --warmup 1 --winner"
Enter
Sleep 20s
Type "q"
Sleep 4s
