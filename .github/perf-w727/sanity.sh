#!/usr/bin/env bash
# The fast builds differ as W7-27 says: in a small project where the config
# excludes src/api.ts and src/api/index.ts is indexed, `./api` resolves to
# src/api/index.ts with the candidate (import candidates are indexed files)
# and not with 654c438 (base), where the disk-only src/api.ts wins.
# usage: sanity.sh <private-dir>
set -euo pipefail
dir=$1
work=$(mktemp -d "$dir/sanity.XXXXXX")
export CODEGRAPH_MCP_REGISTRY_DIR="$work/registry" CODEGRAPH_NO_DAEMON=1 CODEGRAPH_NO_WATCH=1
for side in cand base; do
  p=$work/$side
  mkdir -p "$p/src/api" "$p/.codegraph"
  printf '[app]\nname = "sanity"\n\n[indexing]\nexclude = ["src/api.ts"]\n' > "$p/.codegraph/config.toml"
  printf "import { helper } from './api';\n\nexport function main(): number {\n  return helper();\n}\n" > "$p/src/main.ts"
  printf 'export function helper(): number {\n  return 1;\n}\n' > "$p/src/api/index.ts"
  printf 'export function helper(): number {\n  return 2;\n}\n' > "$p/src/api.ts"
  "$dir/bin-$side/codegraph" init "$p" > /dev/null 2>&1
  db="file:$p/.codegraph/codegraph.db?immutable=1"
  n=$(sqlite3 "$db" "SELECT count(*) FROM edges WHERE kind = 'imports' AND target = 'file:src/api/index.ts'")
  echo "$side: imports into src/api/index.ts $n"
done
rm -rf "$work"
