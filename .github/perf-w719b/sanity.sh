#!/usr/bin/env bash
# The fast builds differ as W7-19b says: on the servers fixture (this
# checkout's crates/codegraph-bench/fixtures/servers) the candidate makes
# Flask's route `GET /hello` (`flask/app.py`) and W7-19a does not.
# usage: sanity.sh <private-dir> <fixture-dir>
set -euo pipefail
dir=$1
fixture=$2
work=$(mktemp -d "$dir/sanity.XXXXXX")
export CODEGRAPH_MCP_REGISTRY_DIR="$work/registry" CODEGRAPH_NO_DAEMON=1 CODEGRAPH_NO_WATCH=1
for side in cand base; do
  mkdir -p "$work/$side"
  cp -a "$fixture/." "$work/$side/"
  "$dir/bin-$side/codegraph" init "$work/$side" > /dev/null 2>&1
  echo "$side routes named GET /hello: $(sqlite3 "file:$work/$side/.codegraph/codegraph.db?immutable=1" "SELECT count(*) FROM nodes WHERE kind = 'route' AND name = 'GET /hello'")"
done
rm -rf "$work"
