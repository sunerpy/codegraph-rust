"""Summary of W7-19e's CI perf times file (cand = the W7-19e candidate
commit's fast release build, base = 97d5b2e's, W7-19d) under the frozen
method: medians over the VALID rounds only (a round is valid when the
1-minute load before each of its samples is below 8, as the script's `round`
lines record); 9 valid rounds needed per measured op.
Lines (OD-14, plan 2.7): cold init median at most 1.10 x base (feedbin,
maybe, solidus, repo, librechat; above it, the growth must be explained
by rows and the normalized throughput, canonical rows per second, must stay
at least 0.9 x base), the one-file sync median at most 1.15 x base (all
six), the peak RSS median at most 1.15 x base (each measured op). A corpus
whose graphs DIFFER in hashes.txt gets no verdict. feedbin-ctl's single
init per side is reference only.

One runner measures the corpora named after the two files; the verdict
covers those only.

usage: python3 -I summary.py <times.txt> <hashes.txt> <corpus>...
"""
import collections
import statistics
import sys

samples = collections.defaultdict(dict)  # (op, corpus, round) -> side -> (wall, rss_kb, load)
rounds = collections.defaultdict(dict)  # (op, corpus) -> round -> (state, max load)
load = None
for line in open(sys.argv[1]):
    parts = line.split()
    if not parts:
        continue
    if parts[0] == "load":
        load = float(parts[1])
    elif parts[0] == "wall":
        op, corpus, side, rnd = parts[1], parts[2], parts[3], parts[4]
        wall = float(parts[5])
        assert parts[6] == "rss_kb", line
        samples[(op, corpus, rnd)][side] = (wall, int(parts[7]), load)
    elif parts[0] == "round":
        rounds[(parts[1], parts[2])][parts[3]] = (parts[4], float(parts[5]))

rows = collections.defaultdict(set)  # (corpus, side) -> init row counts
graphs = {}
for line in open(sys.argv[2]):
    parts = line.split()
    if parts and parts[0] == "hash":
        corpus, side, tag = parts[1].split(".", 2)
        if tag.startswith("init"):
            rows[(corpus, side)].add(int(parts[3]))
    elif parts and parts[0] == "graphs":
        graphs[(parts[1], parts[2])] = parts[3]

LINES = {"init": 1.10, "sync": 1.15}
RSS_LINE = 1.15
plan = [("feedbin", "init"), ("feedbin", "sync"), ("feedbin-ctl", "sync"),
        ("maybe", "init"), ("maybe", "sync"), ("solidus", "init"), ("solidus", "sync"),
        ("repo", "init"), ("repo", "sync"), ("librechat", "init"), ("librechat", "sync")]
only = set(sys.argv[3:])
assert only and only <= {c for c, _ in plan}, only
plan = [(c, op) for c, op in plan if c in only]
ok_all = True
for corpus, op in plan:
    state = {side: graphs.get((corpus, side)) for side in ("cand", "base")}
    if any(v != "agree" for v in state.values()):
        print(f"{corpus} {op}: NO VERDICT, graphs {state}")
        ok_all = False
        continue
    rs = rounds.get((op, corpus), {})
    valid = sorted((r for r, (s, _) in rs.items() if s == "valid"), key=int)
    void = [(r, m) for r, (s, m) in sorted(rs.items(), key=lambda kv: int(kv[0])) if s == "void"]
    print(f"{corpus} {op}: {len(rs)} rounds, {len(valid)} valid, void {void}")
    if len(valid) < 9:
        print(f"  INCOMPLETE: {len(valid)} valid of 9 needed")
        ok_all = False
        continue
    med, rss, loads = {}, {}, {}
    for side in ("base", "cand"):
        med[side] = statistics.median(samples[(op, corpus, r)][side][0] for r in valid)
        rss[side] = statistics.median(samples[(op, corpus, r)][side][1] for r in valid)
        ls = [samples[(op, corpus, r)][side][2] for r in valid]
        loads[side] = (min(ls), max(ls))
        print(f"  {side}: median {med[side]:.4f} s, peak RSS median {rss[side] / 1024:.1f} MiB, load {loads[side][0]:.2f}-{loads[side][1]:.2f}")
    ratio = med["cand"] / med["base"]
    rss_ratio = rss["cand"] / rss["base"]
    within = ratio <= LINES[op]
    note = ""
    if op == "init" and not within:
        rb, rc = rows[(corpus, "base")], rows[(corpus, "cand")]
        if len(rb) == 1 and len(rc) == 1:
            tb, tc = next(iter(rb)) / med["base"], next(iter(rc)) / med["cand"]
            note = f"; rows base {next(iter(rb))} cand {next(iter(rc))}, throughput ratio {tc / tb:.3f} (line 0.9)"
            within = tc / tb >= 0.9 and next(iter(rc)) > next(iter(rb))
    rss_within = rss_ratio <= RSS_LINE
    print(f"  cand/base {ratio:.3f} (line {LINES[op]}){note}; RSS {rss_ratio:.3f} (line {RSS_LINE})"
          f" -> {'within' if within and rss_within else 'OVER'}")
    ok_all = ok_all and within and rss_within
for corpus in [c for c in ("feedbin-ctl",) if c in only]:
    ref = samples.get(("init", corpus, "0"), {})
    if ref:
        print(f"{corpus} init (one sample per side, reference only): "
              + ", ".join(f"{s} {ref[s][0]:.3f} s {ref[s][1] / 1024:.1f} MiB load {ref[s][2]:.2f}" for s in ("base", "cand") if s in ref))
print("VERDICT", "within" if ok_all else "NOT WITHIN or INCOMPLETE")
