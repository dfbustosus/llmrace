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

# The MCP server the way an agent sees it: tools/list, then a race
# returned as a tool result. Offline, no API key.
Type "bash examples/mcp-demo.sh"
Enter
Sleep 10s
