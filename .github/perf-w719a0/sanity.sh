#!/usr/bin/env bash
# The fast builds differ as the commits say: on the servers fixture (this
# checkout's crates/codegraph-bench/fixtures/servers) the W7-19a candidate
# makes the Express mount route `USE /api`, and neither W7-19a0 nor 6841a6d
# does (W7-19a0 changes no graph). The canonical graphs of every corpus
# (perf.sh) check the rest: W7-19a0's must equal 6841a6d's.
# usage: sanity.sh <private-dir> <fixture-dir>
set -euo pipefail
dir=$1
fixture=$2
work=$(mktemp -d "$dir/sanity.XXXXXX")
export CODEGRAPH_MCP_REGISTRY_DIR="$work/registry" CODEGRAPH_NO_DAEMON=1 CODEGRAPH_NO_WATCH=1
for side in cand pre base; do
  mkdir -p "$work/$side"
  cp -a "$fixture/." "$work/$side/"
  "$dir/bin-$side/codegraph" init "$work/$side" > /dev/null 2>&1
  echo "$side routes named USE /api: $(sqlite3 "file:$work/$side/.codegraph/codegraph.db?immutable=1" "SELECT count(*) FROM nodes WHERE kind = 'route' AND name = 'USE /api'")"
done
rm -rf "$work"
