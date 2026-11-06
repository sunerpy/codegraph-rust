"""Summary of a perf times file of W7-18d r3 over the named
corpora (cand = W7-18d r3, base = W7-18c, pre = fec4fbb; the lines of the
W7-18d cost revision, OD-B22: typescript-large and koel at most 1.15 x
W7-18c, typescript-large at most 1.5 x fec4fbb, ghostfolio at most 40 ms
more than W7-18c) under the cost revision's rule:
medians over the VALID rounds only (a round is valid when the 1-minute load
before each of its samples is below 8, as the script's `round` lines record),
the one-file sync ratios against the plan's lines, the stage-only time (stage
sync minus the no-op sync before it, per valid round), and every void round
with its highest load. A corpus without 9 valid sync rounds or 8 valid stage
rounds is INCOMPLETE: no verdict.

usage: python3 -I perf-summary-18d-r3.py <times.txt> <corpus>...
"""
import collections
import statistics
import sys

samples = collections.defaultdict(dict)  # (op, corpus, round) -> side -> (wall, load)
inits = {}
rounds = collections.defaultdict(dict)  # (op, corpus) -> round -> (state, max)
load = None
for line in open(sys.argv[1]):
    parts = line.split()
    if not parts:
        continue
    if parts[0] == "load":
        load = float(parts[1])
    elif parts[0] == "wall":
        op, corpus, side = parts[1], parts[2], parts[3]
        value = float(parts[-1])
        if op == "init":
            inits[(corpus, side)] = (value, load)
        else:
            samples[(op, corpus, parts[4])].setdefault(side, []).append((value, load))
    elif parts[0] == "round":
        rounds[(parts[1], parts[2])][parts[3]] = (parts[4], float(parts[5]))

# (ratio to base, ratio to pre, increment over base in seconds)
lines = {"typescript-large": (1.15, 1.5, None), "koel": (1.15, None, None), "ghostfolio": (None, None, 0.040)}
need = {"sync": 9, "stage": 8}
ok_all = True
corpora = sys.argv[2:]
assert corpora and all(c in lines for c in corpora), corpora
for corpus in corpora:
    for op in ("sync", "stage"):
        rs = rounds.get((op, corpus), {})
        valid = [r for r, (state, _) in rs.items() if state == "valid"]
        void = [(r, m) for r, (state, m) in rs.items() if state == "void"]
        print(f"{corpus} {op}: {len(rs)} rounds, {len(valid)} valid, void {void}")
        if len(valid) < need[op]:
            print(f"  INCOMPLETE: {len(valid)} valid of {need[op]} needed")
            ok_all = False
            continue
        if op == "sync":
            med = {}
            for side in ("pre", "base", "cand"):
                values = [samples[("sync", corpus, r)][side][0][0] for r in valid]
                loads = [samples[("sync", corpus, r)][side][0][1] for r in valid]
                med[side] = statistics.median(values)
                print(f"  sync {side:4} median {med[side]:.4f} min {min(values):.4f} max {max(values):.4f} load {min(loads):.2f}-{max(loads):.2f}")
            to_base, to_pre, over_base = lines[corpus]
            ratio, increment = med["cand"] / med["base"], med["cand"] - med["base"]
            within = ((to_base is None or ratio <= to_base)
                      and (to_pre is None or med["cand"] / med["pre"] <= to_pre)
                      and (over_base is None or increment <= over_base))
            print(f"  cand/base {ratio:.3f}" + (f" (line {to_base})" if to_base else " (reference)")
                  + f" cand-base {increment * 1000:+.1f} ms" + (f" (line +{over_base * 1000:.0f} ms)" if over_base else " (reference)")
                  + f" cand/pre {med['cand'] / med['pre']:.3f}"
                  + (f" (line {to_pre})" if to_pre else " (reference)") + f" -> {'within' if within else 'OVER'}")
            ok_all = ok_all and within
        else:
            for side in ("base", "cand"):
                diffs = []
                for r in valid:
                    stage = samples[("stage", corpus, r)][side][0][0]
                    noop = samples[("noop", corpus, r)][side][0][0]
                    diffs.append(stage - noop)
                print(f"  stage-only {side}: median {statistics.median(diffs):.4f} (n {len(diffs)}, min {min(diffs):.4f}, max {max(diffs):.4f})")
for (corpus, side), (value, l) in sorted(inits.items()):
    print(f"init {corpus} {side} {value:.3f} s (load {l:.2f})")
print("VERDICT", "within" if ok_all else "NOT within or incomplete")
