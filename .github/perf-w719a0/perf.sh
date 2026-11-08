#!/usr/bin/env bash
# W7-19a0 CI perf: W7-19/a/perf/ci/perf-ci/perf.sh (the frozen method, OD-14,
# plan 2.7, run on a GitHub-hosted runner as OD-B19 allows) with three sides
# instead of two, as W7-18b's three-sided copy: base (6841a6d), pre (the
# W7-19a0 commit on it) and cand (the W7-19a commit on W7-19a0), the fast
# release builds in <private-dir>/bin-{base,pre,cand}. W7-19a0 is judged
# pre against base, W7-19a cand against pre; cand against base is reference.
# A round takes one sample per side, its order cycling through three orders
# ("pre base cand", "base cand pre", "cand pre base"), so in every three
# rounds each side runs once first, once second and once third. Everything
# else is the two-sided method: valid when the 1-minute load before each of
# the round's samples is below 8, 9 valid rounds needed, at most 27 run;
# every sample records its wall time and peak RSS (GNU time %M, KiB); the
# corpora come from the arguments as `corpus:dir:file:comment:kind`, kind
# `full` (cold init rounds, then one-file sync rounds) or `sync` (one timed
# init per side, reference only, then one-file sync rounds).
# Canonical graphs (untimed): each valid init round's index is compressed
# (zstd -1, one thread) into <shots-root> and synced to disk before the next
# timed sample; after a corpus's sync rounds the state the last round left,
# a fresh init of each side's final source and (kind sync) a second init of
# the corpus source are snapshotted too; each snapshot is canonicalized by
# the side's `bench --gen-golden` and its hash (the sha256 of the five
# canonical files' sha256 lines) and row count (nodes + edges +
# unresolved_refs + files) go to hashes.txt. Per side, all inits must agree
# and the last sync state must equal the fresh init of the final source
# (DIFFER otherwise: that corpus gets no verdict). Across sides, pre's init
# and final graphs must equal base's (W7-19a0 changes no graph): a `same`
# line, or `DIFFER`, which gives W7-19a0 no verdict on that corpus.
# usage: perf.sh <private-dir> <evidence-dir> <shots-root> <spec>...
set -euo pipefail
shopt -s inherit_errexit
dir=$1
E=$2
shots_root=$3
shift 3
test "$#" -gt 0
export CODEGRAPH_MCP_REGISTRY_DIR="$dir/registry"
export CODEGRAPH_NO_DAEMON=1 CODEGRAPH_NO_WATCH=1
mkdir -p "$E" "$CODEGRAPH_MCP_REGISTRY_DIR"
SIDES="cand pre base"
declare -A bench=([cand]="$dir/bin-cand/bench" [pre]="$dir/bin-pre/bench" [base]="$dir/bin-base/bench")
declare -A bin=([cand]="$dir/bin-cand/codegraph" [pre]="$dir/bin-pre/codegraph" [base]="$dir/bin-base/codegraph")
for side in $SIDES; do sha256sum "${bin[$side]}" "${bench[$side]}"; done
uptime
times="$E/times.txt"
hashes="$E/hashes.txt"
: > "$times"
: > "$hashes"
shots=$(mktemp -d "$shots_root/w719a0-perf.XXXXXX")
trap 'rm -rf -- "$shots"' EXIT
VOID_LOAD=8
NEED=9
MAX_ROUNDS=27
round_max=0
orders=("pre base cand" "base cand pre" "cand pre base")

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
order_of() { echo "${orders[$(( ($1 - 1) % 3 ))]}"; }
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
  for side in $SIDES; do
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
  for side in $(order_of 1); do
    work="$dir/work/$1/$side"
    rm -rf "$work/.codegraph"
    timed init "$1" "$side" 0 -- "${bin[$side]}" init "$work"
    snap "$1" "$side" "init-0"
  done
}
sync_rounds() { # corpus file comment
  local corpus=$1 file=$2 comment=$3 valid=0 round=0 side work state
  for side in $SIDES; do
    work="$dir/work/$corpus/$side"
    test -f "$work/$file"
    printf '\n%sw719a0 warm-up\n' "$comment" >> "$work/$file"
    "${bin[$side]}" sync "$work" > /dev/null 2>&1
  done
  while [ "$valid" -lt "$NEED" ] && [ "$round" -lt "$MAX_ROUNDS" ]; do
    round=$((round + 1))
    round_max=0
    for side in $(order_of "$round"); do
      work="$dir/work/$corpus/$side"
      printf '\n%sw719a0 sync probe %s\n' "$comment" "$round" >> "$work/$file"
      timed sync "$corpus" "$side" "$round" -- "${bin[$side]}" sync "$work"
    done
    if valid_round; then valid=$((valid + 1)); state=valid; else state=void; fi
    echo "round sync $corpus $round $state $round_max" >> "$times"
  done
  for side in $SIDES; do snap "$corpus" "$side" sync-final; done
}
hash_of() { # key
  awk -v k="$1" '$1 == "hash" && $2 == k { print $3 }' "$hashes"
}
check_graphs() { # corpus source second-init?
  local corpus=$1 src=$2 second=$3 side shot name result inits final pre_inits base_inits
  for side in $SIDES; do
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
  for side in $SIDES; do
    inits=$(awk -v p="$corpus.$side.init" '$1 == "hash" && index($2, p) == 1 { print $3 }' "$hashes" | sort -u | wc -l)
    final=$(hash_of "$corpus.$side.final")
    if [ "$inits" -eq 1 ] && [ -n "$final" ] && [ "$final" = "$(hash_of "$corpus.$side.sync-final")" ]; then
      echo "graphs $corpus $side agree final=$final" >> "$hashes"
    else
      echo "graphs $corpus $side DIFFER distinct_inits=$inits" >> "$hashes"
    fi
  done
  pre_inits=$(awk -v p="$corpus.pre.init" '$1 == "hash" && index($2, p) == 1 { print $3 }' "$hashes" | sort -u | tr '\n' ' ')
  base_inits=$(awk -v p="$corpus.base.init" '$1 == "hash" && index($2, p) == 1 { print $3 }' "$hashes" | sort -u | tr '\n' ' ')
  if [ -n "$pre_inits" ] && [ "$pre_inits" = "$base_inits" ] \
    && [ "$(hash_of "$corpus.pre.final")" = "$(hash_of "$corpus.base.final")" ] \
    && [ "$(hash_of "$corpus.pre.sync-final")" = "$(hash_of "$corpus.base.sync-final")" ]; then
    echo "pre-base $corpus same" >> "$hashes"
  else
    echo "pre-base $corpus DIFFER" >> "$hashes"
  fi
}

for spec in "$@"; do
  IFS=: read -r corpus src file comment kind <<< "$spec"
  test -f "$src/$file"
  setup "$corpus" "$src"
  if [ "$kind" = full ]; then init_rounds "$corpus"; else one_init "$corpus"; fi
  sync_rounds "$corpus" "$file" "$comment"
  if [ "$kind" = full ]; then check_graphs "$corpus" "$src" no; else check_graphs "$corpus" "$src" yes; fi
  rm -rf "$dir/work/$corpus"
done
rm -rf "$dir/work" "$dir/fresh"
uptime
echo DONE
