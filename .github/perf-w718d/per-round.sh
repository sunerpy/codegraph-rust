#!/usr/bin/env bash
# The runner copy of per-round-fresh.sh: per valid sync round of the perf
# run, the source as that round left it, rebuilt (the corpus, the untimed
# warm-up comment and the probe comments of rounds 1..r appended to the
# probed file, byte for byte as perf.sh wrote them), freshly indexed by each
# side's build and canonicalized by its `bench --gen-golden`; the hash must
# equal the one perf.sh recorded for the side's index after that round's
# sync (`hash <corpus>.<side>.sync-<r>`). Void rounds' snapshots were
# discarded by the method and are not compared. The corpus comes from the
# argument, as perf.sh's `corpus:dir:file:comment`.
# usage: per-round.sh <private-dir> <out-dir> <times.txt> <hashes.txt> <tag> <spec>
set -euo pipefail
shopt -s inherit_errexit
dir=$1 out=$2 times=$3 hashes=$4 tag=$5 spec=$6
declare -A bins=([cand]=$dir/bin-18d-fast [base]=$dir/bin-18c-fast [pre]=$dir/bin-before)
export CODEGRAPH_MCP_REGISTRY_DIR="$dir/registry" CODEGRAPH_NO_DAEMON=1 CODEGRAPH_NO_WATCH=1
mkdir -p "$out" "$CODEGRAPH_MCP_REGISTRY_DIR"
for side in cand base pre; do sha256sum "${bins[$side]}/codegraph" "${bins[$side]}/bench"; done > "$out/binaries.txt"
results=$out/results.txt
: > "$results"
IFS=: read -r corpus src file comment <<< "$spec"
rounds=$(awk -v c="$corpus" '$1 == "round" && $2 == "sync" && $3 == c { print $4, $5 }' "$times")
last=$(echo "$rounds" | awk 'END { print $1 }')
base_src="$dir/per-round/$corpus/src"
rm -rf "$dir/per-round/$corpus" && mkdir -p "$base_src"
rsync -a --exclude=.codegraph --exclude=.git "$src/" "$base_src/"
printf '\n%s%s warm-up\n' "$comment" "$tag" >> "$base_src/$file"
for r in $(seq 1 "$last"); do
  printf '\n%s%s sync probe %s\n' "$comment" "$tag" "$r" >> "$base_src/$file"
  state=$(echo "$rounds" | awk -v r="$r" '$1 == r { print $2 }')
  [ "$state" = valid ] || continue
  for side in cand base pre; do
    work="$dir/per-round/$corpus/work-$side"
    rm -rf "$work" && mkdir -p "$work"
    rsync -a "$base_src/" "$work/"
    "${bins[$side]}/codegraph" init "$work" > /dev/null 2>&1
    g="$dir/per-round/$corpus/golden-$side"
    rm -rf "$g"
    "${bins[$side]}/bench" --gen-golden "$work/.codegraph/codegraph.db" "$g" > /dev/null
    fresh=$( (cd "$g" && sha256sum nodes.json edges.json refs.json files.json schema.sql) | sha256sum | cut -d" " -f1)
    recorded=$(awk -v k="$corpus.$side.sync-$r" '$1 == "hash" && $2 == k { print $3 }' "$hashes")
    if [ -n "$recorded" ] && [ "$recorded" = "$fresh" ]; then verdict=agree; else verdict=DIFFER; fi
    echo "round $corpus $side $r sync $recorded fresh $fresh $verdict" | tee -a "$results"
    rm -rf "$work" "$g"
  done
done
rm -rf "$dir/per-round/$corpus"
echo "compared $(grep -c '^round' "$results" || true), differ $(grep -c ' DIFFER$' "$results" || true)" | tee -a "$results"
if grep -q ' DIFFER$' "$results"; then exit 1; fi
echo DONE
