#!/usr/bin/env bash
# Wait until quiet (the 1-minute load below 3 at three checks 20 s apart, no
# cargo or rustc of this user), then run perf.sh on the given corpora and
# summarize them under the frozen method (OD-14), as W7-19b's to W7-19h's
# CI copies do.
# usage: quiet-then-perf.sh <private-dir> <evidence-dir> <shots-root> <spec>...
set -uo pipefail
here=$(cd "$(dirname "$0")" && pwd)
dir=$1
E=$2
shots_root=$3
shift 3
mkdir -p "$E"
quiet() { awk '{exit !($1 < 3)}' /proc/loadavg && ! pgrep -u "$USER" -x 'cargo|rustc' > /dev/null; }
export -f quiet
timeout 1800 bash -c 'n=0; while [ "$n" -lt 3 ]; do if quiet; then n=$((n + 1)); else n=0; fi; sleep 20; done' || { echo "never quiet"; exit 3; }
uptime > "$E/load-before.txt"
timeout 18000 bash "$here/perf.sh" "$dir" "$E" "$shots_root" "$@" > "$E/perf.log" 2>&1
status=$?
echo "exit=$status" >> "$E/perf.log"
if [ "$status" -ne 0 ]; then echo "perf.sh failed ($status): no verdict"; tail -20 "$E/perf.log"; exit 1; fi
grep "^graphs" "$E/hashes.txt"
echo "hashes: $(grep -c "^hash" "$E/hashes.txt")"
corpora=()
for spec in "$@"; do corpora+=("${spec%%:*}"); done
python3 -I "$here/summary.py" "$E/times.txt" "$E/hashes.txt" "${corpora[@]}" | tee "$E/summary.txt"
