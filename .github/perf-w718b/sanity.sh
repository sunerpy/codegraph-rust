#!/usr/bin/env bash
# The fast builds differ as W7-18b says: on one SvelteKit project (a page
# `/settings` with a server loader's `load`), W7-18a's draws no page or loader
# edge and W7-18b's the `sveltekit-page` edge from the route to the page and
# the `sveltekit-load` edge from the page to `load`.
# usage: sanity.sh <private-dir>
set -euo pipefail
dir=$1
work=$(mktemp -d "$dir/sanity.XXXXXX")
export CODEGRAPH_MCP_REGISTRY_DIR="$work/registry" CODEGRAPH_NO_DAEMON=1 CODEGRAPH_NO_WATCH=1
declare -A bin=([18a]="$dir/bin-18a-fast/codegraph" [18b]="$dir/bin-18b-fast/codegraph")
for side in 18a 18b; do
  mkdir -p "$work/$side/src/routes/settings"
  printf '{"name":"app","devDependencies":{"@sveltejs/kit":"2","svelte":"5"}}\n' > "$work/$side/package.json"
  printf '<h1>Settings</h1>\n' > "$work/$side/src/routes/settings/+page.svelte"
  printf 'export function load() {\n  return {}\n}\n' > "$work/$side/src/routes/settings/+page.server.js"
  "${bin[$side]}" init "$work/$side" > /dev/null 2>&1
  printf '%s: marker %s, edges ' "$side" "$(sqlite3 "file:$work/$side/.codegraph/codegraph.db?immutable=1" "SELECT value FROM project_metadata WHERE key = 'synthesis_version'")"
  sqlite3 "file:$work/$side/.codegraph/codegraph.db?immutable=1" \
    "SELECT json_extract(e.metadata, '\$.synthesizedBy') || ' ' || s.name || ' -> ' || t.name FROM edges e JOIN nodes s ON s.id = e.source JOIN nodes t ON t.id = e.target WHERE json_extract(e.metadata, '\$.synthesizedBy') IN ('sveltekit-page', 'sveltekit-load') ORDER BY 1" | tr '\n' ';'
  printf '\n'
done
rm -rf "$work"
