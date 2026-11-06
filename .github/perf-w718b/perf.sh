#!/usr/bin/env bash
# The GitHub-hosted runner copy (owner 2026-11-05: measure on CI when the
# local machine never stays quiet). Two changes for the runner, as W7-16c:
# the snapshots go to a disk directory (<shots-root>; /dev/shm cannot hold
# them), each synced to disk before the next timed sample, and the corpora
# come from the arguments, each as `corpus:dir:file:comment` (the local
# scripts' spec). The method below is unchanged.
# W7-18b perf, the frozen method (W7-16a cost revision) with plan 2.7's
# canonical-graph check, as perf-18b.sh: per corpus one init each of the
# candidate (cand, bin-18b-fast), W7-18a (base, bin-18a-fast) and fec4fbb
# (pre, bin-before), all fast
# release builds; one untimed warm-up one-file sync per side; then one-file
# sync rounds, one sample per side, the order cycling through three orders,
# until 9 rounds are valid (at most 27); then stage-only rounds of W7-16b2
# and the candidate in ABBA order (a no-op sync, the marker deleted, the
# timed stage sync), until 8 rounds are valid (at most 24). A round is valid
# when the 1-minute load before each of its samples is below 8. Lines (the
# W7-18 pre-start revision): the candidate's one-file sync at most 1.15 x
# W7-18a's on each corpus, and on typescript-large at most 1.5 x fec4fbb's.
# Canonical graphs (untimed): after every timed sample the side's index is
# compressed (zstd -1, one thread) into a snapshot in /dev/shm; once a corpus
# is measured, each snapshot of a valid round, each init, a second init of
# the same source per side and a fresh init of each side's final source are
# canonicalized by the side's `bench --gen-golden`, and their hash (the
# sha256 of the five canonical files' sha256 lines) is written to
# hashes.txt. Per side, the two inits, and the state the last sync round
# left, every valid stage round and the fresh init of the final source, must
# agree; otherwise the run stops with no verdict.
# usage: perf.sh <private-dir> <evidence-dir> <shots-root> <spec>...
set -euo pipefail
shopt -s inherit_errexit
dir=$1
E=$2
shots_root=$3
shift 3
test "$#" -gt 0
pre=$dir/bin-before
base=$dir/bin-18a-fast
export CODEGRAPH_MCP_REGISTRY_DIR="$dir/registry"
export CODEGRAPH_NO_DAEMON=1 CODEGRAPH_NO_WATCH=1
mkdir -p "$E" "$CODEGRAPH_MCP_REGISTRY_DIR"
declare -A bench=([cand]="$dir/bin-18b-fast/bench" [base]="$base/bench" [pre]="$pre/bench")
declare -A bin=([cand]="$dir/bin-18b-fast/codegraph" [base]="$base/codegraph" [pre]="$pre/codegraph")
declare -A version=([cand]=15 [base]=14)
sha256sum "${bin[cand]}" "${bin[base]}" "${bin[pre]}"
uptime
times="$E/times.txt"
hashes="$E/hashes.txt"
: > "$times"
: > "$hashes"
shots=$(mktemp -d "$shots_root/w718b-perf.XXXXXX")
trap 'rm -rf -- "$shots"' EXIT
snap() { # corpus side tag: compress the side's index (untimed)
  zstd -q -1 -T1 -o "$shots/$1.$2.$3.db.zst" "$dir/perf-18a/$1/$2/.codegraph/codegraph.db"
  sync "$shots/$1.$2.$3.db.zst"
}
canonical() { # side snapshot: the canonical graph's hash
  local out
  out=$(mktemp -d "$shots/golden.XXXXXX")
  zstd -q -d -o "$out/graph.db" "$2"
  "${bench[$1]}" --gen-golden "$out/graph.db" "$out/g" > /dev/null
  (cd "$out/g" && sha256sum nodes.json edges.json refs.json files.json schema.sql) | sha256sum | cut -d" " -f1
  rm -rf -- "$out"
}
fresh() { # corpus side source tag: a fresh init of `source`, snapshotted
  local work="$dir/perf-18a-fresh/$1/$2"
  rm -rf "$work" && mkdir -p "$work"
  rsync -a --exclude=.codegraph --exclude=.git "$3/" "$work/"
  "${bin[$2]}" init "$work" > /dev/null 2>&1
  zstd -q -1 -T1 -o "$shots/$1.$2.$4.db.zst" "$work/.codegraph/codegraph.db"
  sync "$shots/$1.$2.$4.db.zst"
  rm -rf "$work"
}
VOID_LOAD=8
round_max=0
timed() { # label... -- command...
  local labels=() started ended load
  while [ "$1" != "--" ]; do labels+=("$1"); shift; done
  shift
  load=$(cut -d" " -f1 /proc/loadavg)
  printf "load %s\n" "$load" >> "$times"
  round_max=$(awk -v a="$round_max" -v b="$load" 'BEGIN { print (b > a) ? b : a }')
  started=$EPOCHREALTIME
  "$@" > /dev/null 2>&1
  ended=$EPOCHREALTIME
  awk -v label="${labels[*]}" -v a="$started" -v b="$ended" \
    'BEGIN { printf "wall %s %.6f\n", label, b - a }' >> "$times"
}
valid_round() { awk -v m="$round_max" -v v="$VOID_LOAD" 'BEGIN { exit !(m < v) }'; }
orders=("pre base cand" "base cand pre" "cand pre base")
for spec in "$@"; do
  IFS=: read -r corpus src file comment <<< "$spec"
  test -f "$src/$file"
  for side in cand base pre; do
    work="$dir/perf-18a/$corpus/$side"
    rm -rf "$work" && mkdir -p "$work"
    rsync -a --exclude=.codegraph --exclude=.git "$src/" "$work/"
    timed init "$corpus" "$side" -- "${bin[$side]}" init "$work"
    snap "$corpus" "$side" init
  done
  for side in cand base pre; do
    work="$dir/perf-18a/$corpus/$side"
    printf '\n%sw718b warm-up\n' "$comment" >> "$work/$file"
    "${bin[$side]}" sync "$work" > /dev/null 2>&1
  done
  valid=0
  round=0
  while [ "$valid" -lt 9 ] && [ "$round" -lt 27 ]; do
    round=$((round + 1))
    round_max=0
    for side in ${orders[$(( (round - 1) % 3 ))]}; do
      work="$dir/perf-18a/$corpus/$side"
      printf '\n%sw718b sync probe %s\n' "$comment" "$round" >> "$work/$file"
      timed sync "$corpus" "$side" "$round" -- "${bin[$side]}" sync "$work"
      snap "$corpus" "$side" "sync-$round"
    done
    if valid_round; then valid=$((valid + 1)); state=valid; else state=void; fi
    echo "round sync $corpus $round $state $round_max" >> "$times"
    if [ "$state" = void ]; then rm -f "$shots/$corpus".*".sync-$round.db.zst"; fi
  done
  # The state the last round left, void or valid, which the stage rounds
  # keep (they change no source).
  for side in cand base pre; do snap "$corpus" "$side" sync-final; done
  valid=0
  round=0
  while [ "$valid" -lt 8 ] && [ "$round" -lt 24 ]; do
    round=$((round + 1))
    round_max=0
    if [ $(( round % 4 )) -eq 1 ] || [ $(( round % 4 )) -eq 0 ]; then order="base cand"; else order="cand base"; fi
    for side in $order; do
      work="$dir/perf-18a/$corpus/$side"
      db="$work/.codegraph/codegraph.db"
      timed noop "$corpus" "$side" "$round" -- "${bin[$side]}" sync "$work"
      sqlite3 "$db" "DELETE FROM project_metadata WHERE key = 'synthesis_version';"
      if [ -e "$db-wal" ] || [ -e "$db-shm" ]; then
        echo "sidecar left beside $db" >&2
        exit 1
      fi
      timed stage "$corpus" "$side" "$round" -- "${bin[$side]}" sync "$work"
      test "$(sqlite3 "file:$db?immutable=1" "SELECT value FROM project_metadata WHERE key = 'synthesis_version'")" = "${version[$side]}"
      snap "$corpus" "$side" "stage-$round"
    done
    if valid_round; then valid=$((valid + 1)); state=valid; else state=void; fi
    echo "round stage $corpus $round $state $round_max" >> "$times"
    if [ "$state" = void ]; then rm -f "$shots/$corpus".*".stage-$round.db.zst"; fi
  done
  # Untimed: a second init of the corpus source and a fresh init of each
  # side's final source, then every snapshot's canonical hash.
  for side in cand base pre; do
    fresh "$corpus" "$side" "$src" init2
    fresh "$corpus" "$side" "$dir/perf-18a/$corpus/$side" final
  done
  for shot in "$shots/$corpus".*.db.zst; do
    name=$(basename "$shot" .db.zst)
    side=$(echo "$name" | cut -d. -f2)
    echo "hash $name $(canonical "$side" "$shot")" >> "$hashes"
    rm -f "$shot"
  done
  for side in cand base pre; do
    of() { awk -v k="$corpus.$side.$1" '$1 == "hash" && $2 == k { print $3 }' "$hashes"; }
    init=$(of init)
    test -n "$init" && test "$init" = "$(of init2)" || { echo "init graphs differ: $corpus $side" >&2; exit 1; }
    final=$(of final)
    test -n "$final" && test "$final" = "$(of sync-final)" || { echo "the last sync differs from a fresh init: $corpus $side" >&2; exit 1; }
    for stage in $(awk -v p="$corpus.$side.stage-" '$1 == "hash" && index($2, p) == 1 { print $3 }' "$hashes" | sort -u); do
      test "$stage" = "$final" || { echo "a stage round differs from the fresh init: $corpus $side" >&2; exit 1; }
    done
    echo "graphs $corpus $side agree $final" >> "$hashes"
  done
done
rm -rf "$dir/perf-18a" "$dir/perf-18a-fresh"
uptime
echo DONE
