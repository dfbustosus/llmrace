#!/usr/bin/env bash
# The llmrace MCP server, the way an agent sees it. No API key needed —
# this races mock providers offline. Requires jq only for pretty-printing.
set -euo pipefail

session() {
  printf '%s\n' \
    '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' \
    '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
    "$1" | llmrace --mcp
}

echo '$ tools/list — what an agent can call'
session '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' \
  | jq -r 'select(.result.tools) | .result.tools[] | "  " + .name + " — " + .description'

echo
echo '$ tools/call llmrace_race — two mock lanes, offline'
session '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"llmrace_race","arguments":{"providers":["mock","mock"],"runs":1,"warmup":0,"winner":true}}}' \
  | jq -r 'select(.result.content) | .result.content[0].text' \
  | head -14
