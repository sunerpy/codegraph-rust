"""Summary of the three-sided W7-19a0 perf times file under the frozen method
(OD-14, plan 2.7): base = 6841a6d, pre = the W7-19a0 commit on it, cand = the
W7-19a commit on W7-19a0, each a fast release build. Medians over the VALID
rounds only (a round is valid when the 1-minute load before each of its
samples is below 8, as the script's `round` lines record); 9 valid rounds
needed per measured op.
Two judgments, each against the commit before it:
  W7-19a0: pre / base; its graphs must equal base's (`pre-base ... same`).
  W7-19a:  cand / pre.
cand / base is printed for reference. Lines for each: cold init median at
most 1.10 x (librechat, repo; above it, the growth must be explained by rows
and the normalized throughput, canonical rows per second, must stay at least
0.9 x), the one-file sync median at most 1.15 x (every corpus), the peak RSS
median at most 1.15 x (each measured op). A corpus whose graphs DIFFER for a
side in hashes.txt gets no verdict. The single init per side of the `sync`
corpora (koel, feedbin, librechat-routes) is reference only.
One runner measures the corpora named after the two files; the verdicts
cover those only.

usage: python3 -I summary.py <times.txt> <hashes.txt> <corpus>...
"""
import collections
import statistics
import sys

SIDES = ("base", "pre", "cand")
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
pre_base = {}
for line in open(sys.argv[2]):
    parts = line.split()
    if parts and parts[0] == "hash":
        corpus, side, tag = parts[1].split(".", 2)
        if tag.startswith("init"):
            rows[(corpus, side)].add(int(parts[3]))
    elif parts and parts[0] == "graphs":
        graphs[(parts[1], parts[2])] = parts[3]
    elif parts and parts[0] == "pre-base":
        pre_base[parts[1]] = parts[2]

LINES = {"init": 1.10, "sync": 1.15}
RSS_LINE = 1.15
plan = [("librechat", "init"), ("librechat", "sync"), ("repo", "init"), ("repo", "sync"),
        ("koel", "sync"), ("feedbin", "sync"), ("librechat-routes", "sync")]
only = set(sys.argv[3:])
assert only and only <= {c for c, _ in plan}, only
plan = [(c, op) for c, op in plan if c in only]
ok = {"W7-19a0": True, "W7-19a": True}


def judge(corpus, op, med, rss, new, old):
    ratio = med[new] / med[old]
    rss_ratio = rss[new] / rss[old]
    within = ratio <= LINES[op]
    note = ""
    if op == "init" and not within:
        ro, rn = rows[(corpus, old)], rows[(corpus, new)]
        if len(ro) == 1 and len(rn) == 1:
            to, tn = next(iter(ro)) / med[old], next(iter(rn)) / med[new]
            note = (f"; rows {old} {next(iter(ro))} {new} {next(iter(rn))},"
                    f" throughput ratio {tn / to:.3f} (line 0.9)")
            within = tn / to >= 0.9 and next(iter(rn)) > next(iter(ro))
    rss_within = rss_ratio <= RSS_LINE
    return ratio, rss_ratio, note, within and rss_within


for corpus, op in plan:
    state = {side: graphs.get((corpus, side)) for side in SIDES}
    if any(v != "agree" for v in state.values()):
        print(f"{corpus} {op}: NO VERDICT, graphs {state}")
        ok = {k: False for k in ok}
        continue
    rs = rounds.get((op, corpus), {})
    valid = sorted((r for r, (s, _) in rs.items() if s == "valid"), key=int)
    void = [(r, m) for r, (s, m) in sorted(rs.items(), key=lambda kv: int(kv[0])) if s == "void"]
    print(f"{corpus} {op}: {len(rs)} rounds, {len(valid)} valid, void {void}")
    if len(valid) < 9:
        print(f"  INCOMPLETE: {len(valid)} valid of 9 needed")
        ok = {k: False for k in ok}
        continue
    med, rss = {}, {}
    for side in SIDES:
        med[side] = statistics.median(samples[(op, corpus, r)][side][0] for r in valid)
        rss[side] = statistics.median(samples[(op, corpus, r)][side][1] for r in valid)
        ls = [samples[(op, corpus, r)][side][2] for r in valid]
        print(f"  {side}: median {med[side]:.4f} s, peak RSS median {rss[side] / 1024:.1f} MiB,"
              f" load {min(ls):.2f}-{max(ls):.2f}")
    for name, new, old in (("W7-19a0", "pre", "base"), ("W7-19a", "cand", "pre")):
        ratio, rss_ratio, note, within = judge(corpus, op, med, rss, new, old)
        same = ""
        if name == "W7-19a0":
            graph = pre_base.get(corpus)
            same = f"; graphs {graph}"
            within = within and graph == "same"
        print(f"  {name} {new}/{old} {ratio:.3f} (line {LINES[op]}){note}; RSS {rss_ratio:.3f}"
              f" (line {RSS_LINE}){same} -> {'within' if within else 'OVER'}")
        ok[name] = ok[name] and within
    ref = med["cand"] / med["base"]
    print(f"  reference cand/base {ref:.3f}; RSS {rss['cand'] / rss['base']:.3f}")
for corpus in [c for c in ("koel", "feedbin", "librechat-routes") if c in only]:
    ref = samples.get(("init", corpus, "0"), {})
    if ref:
        print(f"{corpus} init (one sample per side, reference only): "
              + ", ".join(f"{s} {ref[s][0]:.3f} s {ref[s][1] / 1024:.1f} MiB load {ref[s][2]:.2f}"
                          for s in SIDES if s in ref))
for name in ("W7-19a0", "W7-19a"):
    print("VERDICT", name, "within" if ok[name] else "NOT WITHIN or INCOMPLETE")
