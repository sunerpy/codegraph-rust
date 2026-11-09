#!/usr/bin/env bash
# W7-19c's second CI perf run (cost revision, OD-B25): on one runner, the
# candidate's one-file sync of a Play routes file against the base's
# one-file sync of a controller, which both sides index. The base (W7-19b,
# 9a6a45d) indexes no routes file, so its sync after a routes edit has
# nothing to do; the candidate indexes routes files as YAML (upstream's
# isPlayRoutesFile), where the same edit is an ordinary one-file sync.
# Four ops per round, each one timed sync after appending one comment line,
# as perf.sh does: cand-routes and base-ctl (the judged pair), cand-ctl and
# base-routes (reference only). A round runs the four in forward order on
# odd rounds and reversed on even ones, so the judged pair alternates which
# goes first; it is valid when the 1-minute load before each of its samples
# is below 8; 9 valid rounds are needed, at most 27 are run. Every sample
# records its wall time and its peak RSS (GNU time %M, KiB).
# Each side's index comes from one untimed-for-the-verdict init (recorded,
# reference only). Canonical graphs (untimed): after the rounds, each side's
# state is snapshotted, a fresh init of that side's final source too, and
# both are canonicalized by the side's `bench --gen-golden`; the last sync
# state must equal the fresh init of the final source, else DIFFER and no
# verdict.
# usage: perf-mix.sh <private-dir> <evidence-dir> <shots-root> <spec>
# spec: corpus:dir:routes-file:routes-comment:controller-file:controller-comment
set -euo pipefail
shopt -s inherit_errexit
dir=$1
E=$2
shots_root=$3
spec=$4
export CODEGRAPH_MCP_REGISTRY_DIR="$dir/registry"
export CODEGRAPH_NO_DAEMON=1 CODEGRAPH_NO_WATCH=1
mkdir -p "$E" "$CODEGRAPH_MCP_REGISTRY_DIR"
declare -A bench=([cand]="$dir/bin-cand/bench" [base]="$dir/bin-base/bench")
declare -A bin=([cand]="$dir/bin-cand/codegraph" [base]="$dir/bin-base/codegraph")
sha256sum "${bin[cand]}" "${bin[base]}" "${bench[cand]}" "${bench[base]}"
uptime
times="$E/times.txt"
hashes="$E/hashes.txt"
: > "$times"
: > "$hashes"
shots=$(mktemp -d "$shots_root/w719c-mix.XXXXXX")
trap 'rm -rf -- "$shots"' EXIT
VOID_LOAD=8
NEED=9
MAX_ROUNDS=27
round_max=0
IFS=: read -r corpus src routes routes_comment ctl ctl_comment <<< "$spec"
test -f "$src/$routes"
test -f "$src/$ctl"
declare -A file=([routes]="$routes" [ctl]="$ctl")
declare -A comment=([routes]="$routes_comment" [ctl]="$ctl_comment")
OPS=("cand routes" "base ctl" "cand ctl" "base routes")

timed() { # label... -- command...
  local labels=() started ended load rss
  while [ "$1" != "--" ]; do labels+=("$1"); shift; done
  shift
  load=$(cut -d" " -f1 /proc/loadavg)
  printf "load %s\n" "$load" >> "$times"
  round_max=$(awk -v a="$round_max" -v b="$load" 'BEGIN { print (b > a) ? b : a }')
  started=$EPOCHREALTIME
  /usr/bin/time -o "$shots/rss" -f '%M' "$@" > /dev/null 2>&1
  ended=$EPOCHREALTIME
  rss=$(tail -n 1 "$shots/rss")
  awk -v label="${labels[*]}" -v a="$started" -v b="$ended" -v r="$rss" \
    'BEGIN { printf "wall %s %.6f rss_kb %s\n", label, b - a, r }' >> "$times"
}
valid_round() { awk -v m="$round_max" -v v="$VOID_LOAD" 'BEGIN { exit !(m < v) }'; }
work_of() { echo "$dir/work/$corpus/$1"; }
edit() { # side op tag
  printf '\n%sw719c sync probe %s\n' "${comment[$2]}" "$3" >> "$(work_of "$1")/${file[$2]}"
}
canonical() { # side snapshot -> "<hash> <rows>"
  local out hash rows
  out=$(mktemp -d "$shots/golden.XXXXXX")
  zstd -q -d -o "$out/graph.db" "$2"
  "${bench[$1]}" --gen-golden "$out/graph.db" "$out/g" > /dev/null
  hash=$(cd "$out/g" && sha256sum nodes.json edges.json refs.json files.json schema.sql | sha256sum | cut -d" " -f1)
  rows=$(sqlite3 "file:$out/graph.db?immutable=1" \
    "SELECT (SELECT count(*) FROM nodes) + (SELECT count(*) FROM edges) + (SELECT count(*) FROM unresolved_refs) + (SELECT count(*) FROM files)")
  rm -rf -- "$out"
  echo "$hash $rows"
}

for side in cand base; do
  work=$(work_of "$side")
  rm -rf "$work" && mkdir -p "$work"
  rsync -a --exclude=.codegraph --exclude=.git "$src/" "$work/"
  timed init "$corpus" "$side" 0 -- "${bin[$side]}" init "$work"
done
# Warm-up: each op once, untimed.
for op in "${OPS[@]}"; do
  read -r side what <<< "$op"
  edit "$side" "$what" warm-up
  "${bin[$side]}" sync "$(work_of "$side")" > /dev/null 2>&1
done
valid=0
round=0
while [ "$valid" -lt "$NEED" ] && [ "$round" -lt "$MAX_ROUNDS" ]; do
  round=$((round + 1))
  round_max=0
  if [ $((round % 2)) -eq 1 ]; then order=("${OPS[@]}"); else order=("${OPS[3]}" "${OPS[2]}" "${OPS[1]}" "${OPS[0]}"); fi
  for op in "${order[@]}"; do
    read -r side what <<< "$op"
    edit "$side" "$what" "$round"
    timed sync "$corpus" "$side-$what" "$round" -- "${bin[$side]}" sync "$(work_of "$side")"
  done
  if valid_round; then valid=$((valid + 1)); state=valid; else state=void; fi
  echo "round sync $corpus $round $state $round_max" >> "$times"
done
for side in cand base; do
  work=$(work_of "$side")
  zstd -q -1 -T1 -o "$shots/$corpus.$side.sync-final.db.zst" "$work/.codegraph/codegraph.db"
  fresh="$dir/fresh/$corpus/$side"
  rm -rf "$fresh" && mkdir -p "$fresh"
  rsync -a --exclude=.codegraph --exclude=.git "$work/" "$fresh/"
  "${bin[$side]}" init "$fresh" > /dev/null 2>&1
  zstd -q -1 -T1 -o "$shots/$corpus.$side.final.db.zst" "$fresh/.codegraph/codegraph.db"
  rm -rf "$fresh"
done
for shot in "$shots/$corpus".*.db.zst; do
  name=$(basename "$shot" .db.zst)
  side=$(echo "$name" | cut -d. -f2)
  echo "hash $name $(canonical "$side" "$shot")" >> "$hashes"
  rm -f -- "$shot"
done
for side in cand base; do
  final=$(awk -v k="$corpus.$side.final" '$1 == "hash" && $2 == k { print $3 }' "$hashes")
  last=$(awk -v k="$corpus.$side.sync-final" '$1 == "hash" && $2 == k { print $3 }' "$hashes")
  if [ -n "$final" ] && [ "$final" = "$last" ]; then
    echo "graphs $corpus $side agree final=$final" >> "$hashes"
  else
    echo "graphs $corpus $side DIFFER" >> "$hashes"
  fi
done
rm -rf "$dir/work" "$dir/fresh"
uptime
echo DONE
