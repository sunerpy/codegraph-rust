#!/usr/bin/env bash
# The GitHub-hosted runner copy of W7-19i's CI perf.sh (OD-B19's method), for
# W7-20: the candidate (cand, the fast release build of the W7-20 candidate
# commit) against 6a58caa8 (base, W7-19i's fast release build) and, when
# SIDES names it, fec4fbb (pre; OD-B20's cumulative line, as W7-18b to
# W7-18d measured typescript-large), each copied into
# <private-dir>/bin-{cand,base,pre}; the snapshots go to a disk directory
# (<shots-root>), each synced to disk before the next timed sample, and the
# corpora come from the arguments, each as `corpus:dir:file:comment:kind`.
# The method is unchanged (OD-14, plan 2.7, the frozen method).
#
# SIDES is "cand base" (the default) or "cand base pre". A round takes one
# sample per side: ABBA order for two sides, the W7-18d rotation (pre base
# cand, base cand pre, cand pre base) for three. A round is valid when the
# 1-minute load before each of its samples is below 8; 9 valid rounds are
# needed, at most 27 are run. Every sample records its wall time and its peak
# RSS (GNU time %M, KiB).
# Kinds: `full` corpora get cold init rounds, then one-file sync rounds on
# the named file, then synthesis-stage rounds (plan 2.7: the stage's time is
# listed on its own; cand and base only, fec4fbb predating the marker): per
# side a no-op sync (timed as `noop`), the synthesis marker deleted, then a timed sync that runs the synthesis stage alone
# (`stage`), the marker checked to be written back with the value it had.
# `sync` corpora get one timed init per side (reference only), then the
# one-file sync rounds.
# Canonical graphs (untimed): each valid init round's index is compressed
# (zstd -1, one thread); after a corpus's rounds the state the last sync
# round left, every valid stage round's state, a fresh init of each side's
# final source and (the sync-only corpora) a second init of the corpus source
# are snapshotted too; each snapshot is canonicalized by the side's
# `bench --gen-golden` and its hash (the sha256 of the five canonical files'
# sha256 lines) and row count (nodes + edges + unresolved_refs + files) are
# written to hashes.txt. Per side, all inits must agree, the last sync state
# must equal the fresh init of the final source, and every stage state must
# equal it too; a disagreement is recorded as DIFFER and the summary gives
# that corpus no verdict.
# usage: SIDES="cand base[ pre]" perf.sh <private-dir> <evidence-dir> <shots-root> <spec>...
set -euo pipefail
shopt -s inherit_errexit
dir=$1
E=$2
shots_root=$3
shift 3
test "$#" -gt 0
read -ra sides <<< "${SIDES:-cand base}"
case "${sides[*]}" in "cand base" | "cand base pre") ;; *) echo "SIDES must be 'cand base' or 'cand base pre'" >&2; exit 2 ;; esac
export CODEGRAPH_MCP_REGISTRY_DIR="$dir/registry"
export CODEGRAPH_NO_DAEMON=1 CODEGRAPH_NO_WATCH=1
mkdir -p "$E" "$CODEGRAPH_MCP_REGISTRY_DIR"
declare -A bench bin
for side in "${sides[@]}"; do
  bench[$side]="$dir/bin-$side/bench"
  bin[$side]="$dir/bin-$side/codegraph"
  sha256sum "${bin[$side]}" "${bench[$side]}"
done
uptime
times="$E/times.txt"
hashes="$E/hashes.txt"
: > "$times"
: > "$hashes"
shots=$(mktemp -d "$shots_root/w720-perf.XXXXXX")
trap 'rm -rf -- "$shots"' EXIT
VOID_LOAD=8
NEED=9
MAX_ROUNDS=27
round_max=0

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
order_of() { # round
  if [ "${#sides[@]}" -eq 2 ]; then
    if [ $(( $1 % 4 )) -eq 1 ] || [ $(( $1 % 4 )) -eq 0 ]; then echo "base cand"; else echo "cand base"; fi
  else
    local orders=("pre base cand" "base cand pre" "cand pre base")
    echo "${orders[$(( ($1 - 1) % 3 ))]}"
  fi
}
snap() { # corpus side tag
  zstd -q -1 -T1 -o "$shots/$1.$2.$3.db.zst" "$dir/work/$1/$2/.codegraph/codegraph.db"
  sync "$shots/$1.$2.$3.db.zst"
}
fresh() { # corpus side source tag: a fresh init of `source`, snapshotted
  local work="$dir/fresh/$1/$2"
  rm -rf "$work" && mkdir -p "$work"
  rsync -a --exclude=.codegraph --exclude=.git "$3/" "$work/"
  "${bin[$2]}" init "$work" > /dev/null 2>&1
  zstd -q -1 -T1 -o "$shots/$1.$2.$4.db.zst" "$work/.codegraph/codegraph.db"
  sync "$shots/$1.$2.$4.db.zst"
  rm -rf "$work"
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
setup() { # corpus source
  local side work
  for side in "${sides[@]}"; do
    work="$dir/work/$1/$side"
    rm -rf "$work" && mkdir -p "$work"
    rsync -a --exclude=.codegraph --exclude=.git "$2/" "$work/"
  done
}
init_rounds() { # corpus
  local corpus=$1 valid=0 round=0 side work state
  while [ "$valid" -lt "$NEED" ] && [ "$round" -lt "$MAX_ROUNDS" ]; do
    round=$((round + 1))
    round_max=0
    for side in $(order_of "$round"); do
      work="$dir/work/$corpus/$side"
      rm -rf "$work/.codegraph"
      timed init "$corpus" "$side" "$round" -- "${bin[$side]}" init "$work"
      snap "$corpus" "$side" "init-$round"
    done
    if valid_round; then valid=$((valid + 1)); state=valid; else state=void; fi
    echo "round init $corpus $round $state $round_max" >> "$times"
    if [ "$state" = void ]; then rm -f "$shots/$corpus".*".init-$round.db.zst"; fi
  done
}
one_init() { # corpus: one timed init per side, reference only
  local side work
  for side in "${sides[@]}"; do
    work="$dir/work/$1/$side"
    rm -rf "$work/.codegraph"
    timed init "$1" "$side" 0 -- "${bin[$side]}" init "$work"
    snap "$1" "$side" "init-0"
  done
}
sync_rounds() { # corpus file comment
  local corpus=$1 file=$2 comment=$3 valid=0 round=0 side work state
  for side in "${sides[@]}"; do
    work="$dir/work/$corpus/$side"
    test -f "$work/$file"
    printf '\n%sw720 warm-up\n' "$comment" >> "$work/$file"
    "${bin[$side]}" sync "$work" > /dev/null 2>&1
  done
  while [ "$valid" -lt "$NEED" ] && [ "$round" -lt "$MAX_ROUNDS" ]; do
    round=$((round + 1))
    round_max=0
    for side in $(order_of "$round"); do
      work="$dir/work/$corpus/$side"
      printf '\n%sw720 sync probe %s\n' "$comment" "$round" >> "$work/$file"
      timed sync "$corpus" "$side" "$round" -- "${bin[$side]}" sync "$work"
    done
    if valid_round; then valid=$((valid + 1)); state=valid; else state=void; fi
    echo "round sync $corpus $round $state $round_max" >> "$times"
  done
  for side in "${sides[@]}"; do snap "$corpus" "$side" sync-final; done
}
stage_rounds() { # corpus: the synthesis stage alone, on the final source
  local corpus=$1 valid=0 round=0 side work db marker state
  while [ "$valid" -lt "$NEED" ] && [ "$round" -lt "$MAX_ROUNDS" ]; do
    round=$((round + 1))
    round_max=0
    for side in $(order_of "$round"); do
      # fec4fbb (pre) predates the synthesis marker: its stage cannot be
      # run alone this way, and the stage is listed for cand and base only.
      if [ "$side" = pre ]; then continue; fi
      work="$dir/work/$corpus/$side"
      db="$work/.codegraph/codegraph.db"
      timed noop "$corpus" "$side" "$round" -- "${bin[$side]}" sync "$work"
      marker=$(sqlite3 "file:$db?immutable=1" "SELECT value FROM project_metadata WHERE key = 'synthesis_version'")
      test -n "$marker"
      sqlite3 "$db" "DELETE FROM project_metadata WHERE key = 'synthesis_version';"
      if [ -e "$db-wal" ] || [ -e "$db-shm" ]; then echo "sidecar left beside $db" >&2; exit 1; fi
      timed stage "$corpus" "$side" "$round" -- "${bin[$side]}" sync "$work"
      test "$(sqlite3 "file:$db?immutable=1" "SELECT value FROM project_metadata WHERE key = 'synthesis_version'")" = "$marker"
      snap "$corpus" "$side" "stage-$round"
    done
    if valid_round; then valid=$((valid + 1)); state=valid; else state=void; fi
    echo "round stage $corpus $round $state $round_max" >> "$times"
    if [ "$state" = void ]; then rm -f "$shots/$corpus".*".stage-$round.db.zst"; fi
  done
}
check_graphs() { # corpus source second-init?
  local corpus=$1 src=$2 second=$3 side shot name result inits final stages
  for side in "${sides[@]}"; do
    fresh "$corpus" "$side" "$dir/work/$corpus/$side" final
    if [ "$second" = yes ]; then fresh "$corpus" "$side" "$src" init2; fi
  done
  for shot in "$shots/$corpus".*.db.zst; do
    name=$(basename "$shot" .db.zst)
    side=$(echo "$name" | cut -d. -f2)
    result=$(canonical "$side" "$shot")
    echo "hash $name $result" >> "$hashes"
    rm -f -- "$shot"
  done
  for side in "${sides[@]}"; do
    inits=$(awk -v p="$corpus.$side.init" '$1 == "hash" && index($2, p) == 1 { print $3 }' "$hashes" | sort -u | wc -l)
    final=$(awk -v k="$corpus.$side.final" '$1 == "hash" && $2 == k { print $3 }' "$hashes")
    stages=$(awk -v p="$corpus.$side.stage-" -v f="$final" '$1 == "hash" && index($2, p) == 1 && $3 != f { print $2 }' "$hashes" | wc -l)
    if [ "$inits" -eq 1 ] && [ -n "$final" ] && [ "$stages" -eq 0 ] \
      && [ "$final" = "$(awk -v k="$corpus.$side.sync-final" '$1 == "hash" && $2 == k { print $3 }' "$hashes")" ]; then
      echo "graphs $corpus $side agree final=$final" >> "$hashes"
    else
      echo "graphs $corpus $side DIFFER distinct_inits=$inits stages_unlike_final=$stages" >> "$hashes"
    fi
  done
}

for spec in "$@"; do
  IFS=: read -r corpus src file comment kind <<< "$spec"
  test -f "$src/$file"
  setup "$corpus" "$src"
  if [ "$kind" = full ]; then init_rounds "$corpus"; else one_init "$corpus"; fi
  sync_rounds "$corpus" "$file" "$comment"
  if [ "$kind" = full ]; then stage_rounds "$corpus"; fi
  if [ "$kind" = full ]; then check_graphs "$corpus" "$src" no; else check_graphs "$corpus" "$src" yes; fi
  rm -rf "$dir/work/$corpus"
done
rm -rf "$dir/work" "$dir/fresh"
uptime
echo DONE
