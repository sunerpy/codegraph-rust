#!/usr/bin/env python3
"""Apply the OD-9 adoption rule to `bench --ab` reports (schema 2).

usage:
  python3 -I od9-verdict.py --target PHASE[,PHASE...] --noise-floor-ms MS
      --profile release|fast [--goldens-ok] [--corpora NAME[,NAME...]]
      [--threads N[,M...]] [--candidate INDEX] [--json] REPORT.json...

`--target` names the phases the strategy changes, the phase its cost moves
into included; the target metric of a run is the sum of their milliseconds.
`wall` stands for the run's wall time. `--threads` names the thread cells every
required corpus must have (default `32,4`; `default` is a cell that left
RAYON_NUM_THREADS unset). `--corpora` defaults to every corpus the reports hold.
`--candidate` picks the report's candidate (default 0, the first `--candidate`).
Cells may come from several reports, a cell from one report only.

A strategy is ADOPTED only when all three rules hold for the candidate:

(a) every required corpus's canonical graph equals the baseline's on all five
    surfaces, and `--goldens-ok` attests that `scripts/regen-goldens.sh --check`
    reported every golden identical and the `golden_reextract` test passed,
    both on the candidate build;
(b) in every required cell the target improves: with B and C the baseline and
    candidate medians over the measured runs (warm-up and void runs left out,
    at least five each) and MAD the unscaled median absolute deviation,
    (B - C) / B >= 0.05 and B - C > 2 * max(MAD_B, MAD_C);
(c) in every required cell nothing else regresses: no other phase both arms
    report, nor the wall time (unless it is the target), nor the peak RSS.
    A regression is C / B > 1.02 together with C - B > 2 * max(MAD_B, MAD_C);
    a phase whose baseline median is below the noise floor is judged by its
    absolute increase alone, C - B > noise floor.

The verdict needs builds of the release profile (OD-B88); `--profile fast`
prints the same evaluation as attribution only.

Exit status: 0 ADOPTED, 1 REJECTED_BY_PROFILE, 3 no verdict (a rule could not
be evaluated, or `--profile fast`), 2 a usage or input error.
"""

import argparse
import json
import sys

IMPROVEMENT_MIN = 0.05
REGRESSION_RATIO_MAX = 1.02
MAD_FACTOR = 2.0
MIN_MEASURED_RUNS = 5
WALL = "wall"


class InputError(Exception):
    """A report that cannot be read as a schema-2 `bench --ab` report."""


def median(values):
    """The median, interpolated between the middle two of an even count, as
    the bench's `metrics::median` computes it."""
    ordered = sorted(values)
    count = len(ordered)
    middle = count // 2
    if count % 2:
        return ordered[middle]
    return ordered[middle - 1] + (ordered[middle] - ordered[middle - 1]) * 0.5


def mad(values):
    """The unscaled median absolute deviation."""
    center = median(values)
    return median([abs(value - center) for value in values])


def thread_label(cell):
    return "default" if cell.get("default_threads") else str(cell["threads"])


def measured(series):
    """The runs a statistic counts: neither warm-up nor void."""
    return [run for run in series["runs"] if not run.get("warmup") and not run.get("void")]


def metric_values(runs, phases):
    """Each run's sum of `phases` (milliseconds), or None when a run lacks one."""
    values = []
    for run in runs:
        if phases == [WALL]:
            values.append(float(run["wall_ms"]))
            continue
        reported = run.get("phase_ms") or {}
        if any(phase not in reported for phase in phases):
            return None
        values.append(sum(float(reported[phase]) for phase in phases))
    return values


def load_reports(paths, candidate):
    """Cells keyed by (corpus, threads label) and graphs by corpus."""
    cells = {}
    graphs = {}
    for path in paths:
        try:
            with open(path, encoding="utf-8") as handle:
                report = json.load(handle)
        except (OSError, ValueError) as error:
            raise InputError(f"{path}: {error}") from error
        if report.get("kind") != "codegraph-ab" or report.get("schema_version") != 2:
            raise InputError(f"{path}: not a schema-2 `bench --ab` report")
        if candidate >= len(report.get("candidates", [])):
            raise InputError(f"{path}: has no candidate {candidate}")
        for corpus in report["corpora"]:
            name = corpus["name"]
            graphs.setdefault(name, []).append((path, corpus["graphs"][candidate]))
            for cell in corpus["cells"]:
                key = (name, thread_label(cell))
                if key in cells:
                    raise InputError(
                        f"cell {name}/{key[1]} is in both {cells[key][0]} and {path}"
                    )
                cells[key] = (path, cell["baseline"], cell["candidates"][candidate]["series"])
    return cells, graphs


def judge_target(label, base_values, cand_values):
    base, cand = median(base_values), median(cand_values)
    spread = MAD_FACTOR * max(mad(base_values), mad(cand_values))
    delta = base - cand
    relative = delta / base if base > 0 else float("nan")
    passed = base > 0 and relative >= IMPROVEMENT_MIN and delta > spread
    reason = (
        f"{label}: target {base:.3f} -> {cand:.3f} ms, "
        f"improvement {relative * 100:.2f}% (needs >= 5%), "
        f"delta {delta:.3f} vs 2*MAD {spread:.3f} (needs >)"
    )
    return passed, reason


def regression(base_values, cand_values, noise_floor):
    """Whether the candidate regressed, and why."""
    base, cand = median(base_values), median(cand_values)
    spread = MAD_FACTOR * max(mad(base_values), mad(cand_values))
    increase = cand - base
    if base < noise_floor:
        return increase > noise_floor, (
            f"{base:.3f} -> {cand:.3f}, increase {increase:.3f} vs noise floor "
            f"{noise_floor:.3f} (below the floor: absolute increase only)"
        )
    ratio = cand / base if base > 0 else float("inf")
    return ratio > REGRESSION_RATIO_MAX and increase > spread, (
        f"{base:.3f} -> {cand:.3f}, ratio {ratio:.4f} (limit 1.02), "
        f"increase {increase:.3f} vs 2*MAD {spread:.3f}"
    )


def evaluate(args):
    targets = [name.strip() for name in args.target.split(",") if name.strip()]
    if not targets:
        raise InputError("--target names no phase")
    if WALL in targets and targets != [WALL]:
        raise InputError("`wall` is a target on its own")
    threads = [label.strip() for label in args.threads.split(",") if label.strip()]
    cells, graphs = load_reports(args.reports, args.candidate)
    corpora = (
        [name.strip() for name in args.corpora.split(",") if name.strip()]
        if args.corpora
        else sorted({name for name, _ in cells})
    )
    failed, missing, passed, notes = [], [], [], []

    # (a) the graphs.
    for name in corpora:
        if name not in graphs:
            missing.append(f"(a) corpus {name} is in no report")
            continue
        for path, graph in graphs[name]:
            if graph["identical"]:
                passed.append(f"(a) {name}: graph identical ({path})")
            else:
                failed.append(
                    f"(a) {name}: graph differs in {', '.join(graph['differing_surfaces'])} ({path})"
                )
    if args.goldens_ok:
        passed.append("(a) goldens and golden_reextract attested (--goldens-ok)")
    else:
        missing.append("(a) no --goldens-ok: the golden checks are not attested")

    for name in corpora:
        for label in threads:
            key = (name, label)
            if key not in cells:
                missing.append(f"cell {name}/{label} is in no report")
                continue
            path, base_series, cand_series = cells[key]
            cell = f"{name}/{label}"
            base_runs, cand_runs = measured(base_series), measured(cand_series)
            if min(len(base_runs), len(cand_runs)) < MIN_MEASURED_RUNS:
                missing.append(
                    f"cell {cell}: {len(base_runs)} and {len(cand_runs)} measured runs "
                    f"(needs {MIN_MEASURED_RUNS} each: six runs, the first dropped)"
                )
                continue

            # (b) the target.
            base_values = metric_values(base_runs, targets)
            cand_values = metric_values(cand_runs, targets)
            if base_values is None or cand_values is None:
                missing.append(f"(b) {cell}: a run does not report {', '.join(targets)}")
            else:
                ok, reason = judge_target(f"(b) {cell}", base_values, cand_values)
                (passed if ok else failed).append(reason)
                if targets != [WALL] and median(base_values) < args.noise_floor_ms:
                    notes.append(f"(b) {cell}: the target is below the noise floor")

            # (c) everything else.
            base_phases = set.intersection(*(set(run.get("phase_ms") or {}) for run in base_runs))
            cand_phases = set.intersection(*(set(run.get("phase_ms") or {}) for run in cand_runs))
            for phase in sorted(base_phases ^ cand_phases):
                notes.append(f"(c) {cell}: {phase} is reported by one arm only")
            others = [phase for phase in sorted(base_phases & cand_phases) if phase not in targets]
            if targets != [WALL]:
                others.append(WALL)
            regressions = []
            for phase in others:
                base_other = metric_values(base_runs, [phase])
                cand_other = metric_values(cand_runs, [phase])
                regressed, reason = regression(base_other, cand_other, args.noise_floor_ms)
                if regressed:
                    regressions.append(f"(c) {cell}: {phase} regressed: {reason}")
            base_rss = [run.get("peak_rss_kb") for run in base_runs]
            cand_rss = [run.get("peak_rss_kb") for run in cand_runs]
            if None in base_rss or None in cand_rss:
                notes.append(f"(c) {cell}: no peak RSS to compare")
            else:
                regressed, reason = regression(
                    [float(kb) for kb in base_rss], [float(kb) for kb in cand_rss], 0.0
                )
                if regressed:
                    regressions.append(f"(c) {cell}: peak RSS (KiB) regressed: {reason}")
            if regressions:
                failed.extend(regressions)
            else:
                passed.append(f"(c) {cell}: none of {len(others)} timings nor the peak RSS regressed")

    if failed:
        verdict = "REJECTED_BY_PROFILE"
    elif missing:
        verdict = "INCOMPLETE"
    else:
        verdict = "ADOPTED"
    return {
        "verdict": verdict,
        "attribution_only": args.profile != "release",
        "target": targets,
        "noise_floor_ms": args.noise_floor_ms,
        "corpora": corpora,
        "threads": threads,
        "failed": failed,
        "missing": missing,
        "passed": passed,
        "notes": notes,
    }


def main(argv):
    parser = argparse.ArgumentParser(
        description="Apply the OD-9 adoption rule to bench --ab reports (schema 2)."
    )
    parser.add_argument("reports", nargs="+", metavar="REPORT.json")
    parser.add_argument("--target", required=True)
    parser.add_argument("--noise-floor-ms", type=float, required=True)
    parser.add_argument("--profile", choices=["release", "fast"], required=True)
    parser.add_argument("--goldens-ok", action="store_true")
    parser.add_argument("--corpora")
    parser.add_argument("--threads", default="32,4")
    parser.add_argument("--candidate", type=int, default=0)
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args(argv)
    if args.noise_floor_ms < 0 or args.candidate < 0:
        parser.error("--noise-floor-ms and --candidate cannot be negative")
    try:
        result = evaluate(args)
    except (InputError, KeyError, TypeError) as error:
        print(f"od9-verdict: {error}", file=sys.stderr)
        return 2
    if args.json:
        print(json.dumps(result, indent=2, sort_keys=True))
    else:
        for heading in ("failed", "missing", "passed", "notes"):
            for line in result[heading]:
                print(f"{heading}: {line}")
        suffix = " (attribution only: --profile fast, OD-B88)" if result["attribution_only"] else ""
        print(f"VERDICT: {result['verdict']}{suffix}")
    if result["attribution_only"]:
        return 3
    return {"ADOPTED": 0, "REJECTED_BY_PROFILE": 1}.get(result["verdict"], 3)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
