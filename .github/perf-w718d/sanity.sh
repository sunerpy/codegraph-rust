#!/usr/bin/env bash
# The fast builds differ as W7-18d says, on top of W7-18c: on one Angular
# project (a component whose inline template binds `(click)="go()"`), W7-18c's
# draws no template edge and W7-18d's the `angular-event` edge from the class
# to `go`; the markers are 16 and 17 (the local fast-build-sanity-18d.sh).
# usage: sanity.sh <private-dir>
set -euo pipefail
dir=$1
work=$(mktemp -d "$dir/sanity.XXXXXX")
export CODEGRAPH_MCP_REGISTRY_DIR="$work/registry" CODEGRAPH_NO_DAEMON=1 CODEGRAPH_NO_WATCH=1
declare -A bin=([18c]="$dir/bin-18c-fast/codegraph" [18d]="$dir/bin-18d-fast/codegraph")
for side in 18c 18d; do
  mkdir -p "$work/$side/src/app"
  printf '{"name":"app","dependencies":{"@angular/core":"^19.0.0"}}\n' > "$work/$side/package.json"
  printf "import { Component } from '@angular/core';\n@Component({ selector: 'app-x', template: \`<button (click)=\"go()\">Go</button>\` })\nexport class XComponent {\n  go(): void {}\n}\n" > "$work/$side/src/app/x.component.ts"
  "${bin[$side]}" init "$work/$side" > /dev/null 2>&1
  printf '%s: marker %s, edges ' "$side" "$(sqlite3 "file:$work/$side/.codegraph/codegraph.db?immutable=1" "SELECT value FROM project_metadata WHERE key = 'synthesis_version'")"
  sqlite3 "file:$work/$side/.codegraph/codegraph.db?immutable=1" \
    "SELECT json_extract(e.metadata, '\$.synthesizedBy') || ' ' || s.name || ' -> ' || t.name FROM edges e JOIN nodes s ON s.id = e.source JOIN nodes t ON t.id = e.target WHERE json_extract(e.metadata, '\$.synthesizedBy') LIKE 'angular-%' ORDER BY 1" | tr '\n' ';'
  printf '\n'
done
rm -rf "$work"
