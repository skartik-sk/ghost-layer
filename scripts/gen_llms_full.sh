#!/usr/bin/env bash
# Regenerates llms-full.txt from the real binary's --help output and the JSON schemas.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build --release -q -p ghost-layer
G=target/release/ghost
{
  cat llms.txt
  echo; echo "---"; echo; echo "# Full command reference (generated from \`ghost <cmd> --help\` by scripts/gen_llms_full.sh)"
  echo; echo '```'; "$G" --help; echo '```'
  for c in init pay up nym status verify ip doctor explain attack report dashboard mcp proof; do
    echo; echo "## ghost $c"; echo; echo '```'; "$G" "$c" --help; echo '```'
  done
  echo; echo "---"; echo; cat docs/json-schema.md
  for f in docs/schemas/*.schema.json; do
    echo; echo "### $(basename "$f")"; echo; echo '```json'; cat "$f"; echo; echo '```'
  done
} > llms-full.txt
wc -l llms-full.txt
