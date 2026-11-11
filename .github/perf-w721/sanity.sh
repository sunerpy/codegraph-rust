#!/usr/bin/env bash
# The fast builds differ as W7-21 says: on a copy of this checkout's mobile
# fixture (crates/codegraph-bench/fixtures/mobile), the candidate makes the
# `rn-export:` node of CameraModule.takePicture and stamps synthesis marker
# 19; 28a3b32e (base, W7-20) makes none and stamps 18.
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
  marker=$(sqlite3 "$db" "SELECT value FROM project_metadata WHERE key = 'synthesis_version'")
  nodes=$(sqlite3 "$db" "SELECT count(*) FROM nodes WHERE id LIKE 'rn-export:%'")
  echo "$side: marker $marker, rn-export nodes $nodes"
done
rm -rf "$work"
