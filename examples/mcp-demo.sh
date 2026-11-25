#!/usr/bin/env bash
# The llmrace MCP server, the way an agent sees it.
#
#   bash examples/mcp-demo.sh                      # two mock lanes, offline
#   bash examples/mcp-demo.sh openai groq          # real providers (env keys)
#
# Requires jq only for pretty-printing.
set -euo pipefail

if [ $# -eq 0 ]; then
  providers_json='["mock","mock"]'
  label="two mock lanes, offline"
else
  providers_json=$(printf '%s\n' "$@" | jq -R . | jq -cs .)
  label="$*"
fi

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
echo "\$ tools/call llmrace_race — ${label}"
session "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/call\",\"params\":{\"name\":\"llmrace_race\",\"arguments\":{\"providers\":${providers_json},\"runs\":2,\"warmup\":1,\"winner\":true}}}" \
  | jq -r 'select(.result.content) | .result.content[0].text' \
  | head -14
