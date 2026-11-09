#!/usr/bin/env bash
# The fast builds differ as W7-19d says: on the servers fixture (this
# checkout's crates/codegraph-bench/fixtures/servers) the candidate makes the
# two Laravel routes of `laravel/routes/web.php` and the Drupal route of
# `drupal/modules/custom/greeting/greeting.routing.yml`, and W7-19c makes
# neither.
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
  echo "$side routes in laravel/routes/web.php: $(sqlite3 "$db" "SELECT count(*) FROM nodes WHERE kind = 'route' AND file_path = 'laravel/routes/web.php'")"
  echo "$side routes in greeting.routing.yml: $(sqlite3 "$db" "SELECT count(*) FROM nodes WHERE kind = 'route' AND file_path = 'drupal/modules/custom/greeting/greeting.routing.yml'")"
done
rm -rf "$work"
