#!/usr/bin/env bash
# The fast builds differ as W7-20 says: on a three-file Next.js project (an
# App Router handler `GET /api/items` and a client `loadItems` that fetches
# it), the candidate draws one `http-client` edge from loadItems to the route
# and stamps synthesis marker 18; 6a58caa8 (base) draws none and stamps 17.
# usage: sanity.sh <private-dir>
set -euo pipefail
dir=$1
work=$(mktemp -d "$dir/sanity.XXXXXX")
export CODEGRAPH_MCP_REGISTRY_DIR="$work/registry" CODEGRAPH_NO_DAEMON=1 CODEGRAPH_NO_WATCH=1
for side in cand base; do
  p=$work/$side
  mkdir -p "$p/app/api/items" "$p/src"
  printf '{\n  "name": "sanity",\n  "dependencies": { "next": "14.2.0", "react": "18.3.0" }\n}\n' > "$p/package.json"
  printf 'export async function GET() {\n  return Response.json([]);\n}\n' > "$p/app/api/items/route.ts"
  printf "export async function loadItems() {\n  const res = await fetch('/api/items');\n  return res.json();\n}\n" > "$p/src/client.ts"
  "$dir/bin-$side/codegraph" init "$p" > /dev/null 2>&1
  db="file:$p/.codegraph/codegraph.db?immutable=1"
  marker=$(sqlite3 "$db" "SELECT value FROM project_metadata WHERE key = 'synthesis_version'")
  edges=$(sqlite3 "$db" "SELECT count(*) FROM edges e JOIN nodes s ON s.id = e.source JOIN nodes t ON t.id = e.target WHERE e.metadata LIKE '%\"synthesizedBy\":\"http-client\"%' AND s.name = 'loadItems' AND t.name = 'GET /api/items'")
  echo "$side: marker $marker, http-client edges $edges"
done
rm -rf "$work"
