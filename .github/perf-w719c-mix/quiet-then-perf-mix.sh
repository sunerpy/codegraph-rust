#!/usr/bin/env bash
# Wait until quiet (the 1-minute load below 3 at three checks 20 s apart, no
# cargo or rustc of this user), then run perf-mix.sh on the given spec and
# summarize it (summary-mix.py), as W7-19c's first CI copy does for
# perf.sh.
# usage: quiet-then-perf-mix.sh <private-dir> <evidence-dir> <shots-root> <spec>
set -uo pipefail
here=$(cd "$(dirname "$0")" && pwd)
dir=$1
E=$2
shots_root=$3
spec=$4
mkdir -p "$E"
quiet() { awk '{exit !($1 < 3)}' /proc/loadavg && ! pgrep -u "$USER" -x 'cargo|rustc' > /dev/null; }
export -f quiet
timeout 1800 bash -c 'n=0; while [ "$n" -lt 3 ]; do if quiet; then n=$((n + 1)); else n=0; fi; sleep 20; done' || { echo "never quiet"; exit 3; }
uptime > "$E/load-before.txt"
timeout 18000 bash "$here/perf-mix.sh" "$dir" "$E" "$shots_root" "$spec" > "$E/perf.log" 2>&1
status=$?
echo "exit=$status" >> "$E/perf.log"
if [ "$status" -ne 0 ]; then echo "perf-mix.sh failed ($status): no verdict"; tail -20 "$E/perf.log"; exit 1; fi
grep "^graphs" "$E/hashes.txt"
python3 -I "$here/summary-mix.py" "$E/times.txt" "$E/hashes.txt" | tee "$E/summary.txt"
