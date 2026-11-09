"""Summary of W7-19c's second CI perf run (perf-mix.sh; cost revision,
OD-B25): medians over the VALID rounds only (a round is valid when the
1-minute load before each of its samples is below 8, as the script's `round`
lines record); 9 valid rounds needed.
Judged pair (one runner): the candidate's one-file sync of the Play routes
file (cand-routes) against the base's one-file sync of the controller
(base-ctl), the median at most 1.15 x, and the peak RSS median at most
1.15 x. cand-ctl and base-routes, and each side's one init, are reference
only. A side whose graphs DIFFER in hashes.txt gets no verdict.

usage: python3 -I summary-mix.py <times.txt> <hashes.txt>
"""
import collections
import statistics
import sys

LINE = 1.15
RSS_LINE = 1.15
LABELS = ["cand-routes", "base-ctl", "cand-ctl", "base-routes"]

samples = collections.defaultdict(dict)  # label -> round -> (secs, rss_kb)
inits = {}
valid_rounds = set()
void_rounds = []
for line in open(sys.argv[1]):
    parts = line.split()
    if not parts:
        continue
    if parts[0] == "wall" and parts[1] == "sync":
        assert parts[6] == "rss_kb", line
        samples[parts[3]][int(parts[4])] = (float(parts[5]), int(parts[7]))
    elif parts[0] == "wall" and parts[1] == "init":
        inits[parts[3]] = (float(parts[5]), int(parts[7]))
    elif parts[0] == "round" and parts[1] == "sync":
        if parts[4] == "valid":
            valid_rounds.add(int(parts[3]))
        else:
            void_rounds.append(int(parts[3]))

agree = {}
for line in open(sys.argv[2]):
    parts = line.split()
    if parts and parts[0] == "graphs":
        agree[parts[2]] = parts[3] == "agree"

ok = len(valid_rounds) >= 9 and agree.get("cand") and agree.get("base")
medians = {}
rss = {}
for label in LABELS:
    rows = [samples[label][r] for r in sorted(valid_rounds) if r in samples[label]]
    if len(rows) != len(valid_rounds):
        ok = False
    if not rows:
        ok = False
        continue
    medians[label] = statistics.median(secs for secs, _ in rows)
    rss[label] = statistics.median(kb for _, kb in rows) / 1024
    print(f"{label}: median {medians[label]:.4f} s over {len(rows)} valid rounds, "
          f"peak RSS median {rss[label]:.1f} MiB")
print(f"rounds: {len(valid_rounds)} valid, void {void_rounds}")
for side, (secs, kb) in sorted(inits.items()):
    print(f"init {side} (one sample, reference only): {secs:.3f} s, {kb / 1024:.1f} MiB")
print("graphs:", ", ".join(f"{side} {'agree' if good else 'DIFFER'}" for side, good in sorted(agree.items())))
if "cand-routes" in medians and "base-ctl" in medians:
    ratio = medians["cand-routes"] / medians["base-ctl"]
    rss_ratio = rss["cand-routes"] / rss["base-ctl"]
    within = ratio <= LINE and rss_ratio <= RSS_LINE
    print(f"judged: cand-routes/base-ctl {ratio:.3f} (line {LINE}); RSS {rss_ratio:.3f} "
          f"(line {RSS_LINE}) -> {'within' if within else 'OVER'}")
    ok = ok and within
for a, b in [("cand-ctl", "base-ctl"), ("cand-routes", "cand-ctl"), ("cand-routes", "base-routes")]:
    if a in medians and b in medians:
        print(f"reference: {a}/{b} {medians[a] / medians[b]:.3f}; RSS {rss[a] / rss[b]:.3f}")
print("VERDICT", "within" if ok else "NOT WITHIN or INCOMPLETE")
