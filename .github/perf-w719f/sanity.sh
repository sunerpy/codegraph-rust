#!/usr/bin/env bash
# The fast builds differ as W7-19f says: on the servers fixture (this
# checkout's crates/codegraph-bench/fixtures/servers) the candidate makes the
# two ASP.NET routes under `aspnet/` (the controller's `[HttpGet]` and the
# minimal API's `MapGet`), and W7-19e makes none.
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
  db="file:$work/$side/.codegraph/codegraph.db?immutable=1"
  echo "$side routes in aspnet/: $(sqlite3 "$db" "SELECT count(*) FROM nodes WHERE kind = 'route' AND file_path LIKE 'aspnet/%'")"
done
rm -rf "$work"
