#!/usr/bin/env bash
# The fast builds differ as W7-19h says: on a copy of the servers fixture
# (this checkout's crates/codegraph-bench/fixtures/servers) given a root
# Cargo.toml, which the Rust resolver is detected by, the candidate makes the
# axum and actix routes under `axum/` and `actix/`, and W7-19g makes none.
# (The fixture itself has no root Cargo.toml, so neither side detects Rust
# on it as it is.)
# usage: sanity.sh <private-dir> <fixture-dir>
set -euo pipefail
dir=$1
fixture=$2
work=$(mktemp -d "$dir/sanity.XXXXXX")
export CODEGRAPH_MCP_REGISTRY_DIR="$work/registry" CODEGRAPH_NO_DAEMON=1 CODEGRAPH_NO_WATCH=1
for side in cand base; do
  mkdir -p "$work/$side"
  cp -a "$fixture/." "$work/$side/"
  test ! -e "$work/$side/Cargo.toml"
  printf '[workspace]\nmembers = []\n' > "$work/$side/Cargo.toml"
  "$dir/bin-$side/codegraph" init "$work/$side" > /dev/null 2>&1
  db="file:$work/$side/.codegraph/codegraph.db?immutable=1"
  echo "$side routes in axum/ and actix/: $(sqlite3 "$db" "SELECT count(*) FROM nodes WHERE kind = 'route' AND (file_path LIKE 'axum/%' OR file_path LIKE 'actix/%')")"
done
rm -rf "$work"
