#!/usr/bin/env bash
# The fast builds differ as W7-19i says: on a copy of the servers fixture
# (this checkout's crates/codegraph-bench/fixtures/servers), whose indexed .go
# files detect Go, the candidate makes the gin route under `gin/` and W7-19h
# makes none.
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
  echo "$side routes in gin/: $(sqlite3 "$db" "SELECT count(*) FROM nodes WHERE kind = 'route' AND file_path LIKE 'gin/%'")"
done
rm -rf "$work"
