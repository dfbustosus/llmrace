#!/bin/sh
# Race every configured provider, save dated reports under reports/.
# Usage: examples/race-all.sh [extra llmrace flags...]
set -eu

OUT_DIR="${LLMRACE_REPORT_DIR:-reports}"
STAMP="$(date -u +%Y-%m-%dT%H%M%SZ)"
mkdir -p "$OUT_DIR"

llmrace --json --csv --record --out "$OUT_DIR/race-$STAMP.md" "$@"
llmrace --trend --days 30
