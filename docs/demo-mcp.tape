Output docs/demo-mcp.gif
Require llmrace
Set FontSize 14
Set Width 1280
Set Height 640
Set Theme "Catppuccin Mocha"
Set WindowBar Rings
Set Padding 18
Set TypingSpeed 10ms
Set CursorBlink true

# The MCP server the way an agent sees it: tools/list, then a real race
# (OpenAI + DeepSeek through OpenRouter) returned as a tool result.
Type "bash examples/mcp-demo.sh openai=gpt-4o-mini openrouter=deepseek/deepseek-chat-v3.1"
Enter
Sleep 14s
