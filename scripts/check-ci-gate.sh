#!/usr/bin/env bash
# check-ci-gate.sh — CI/release gate-integrity guard.
#
# Purpose
#   `CI Success` (job id `ci-success` in .github/workflows/ci.yml) is the single
#   required status check for branch protection AND the job the release workflow's
#   `verify-ci` waits on before a GitHub Release may leave draft. Everything the
#   project promises — "local green ⇒ CI green", "a release only fires on green
#   CI" — rests on that one job being a STRICT gate.
#
#   Two ways it can silently stop being one, neither caught by any Cargo test:
#
#     1. The gate accepts a non-`success` result. `needs.<job>.result` can be
#        `success`, `failure`, `cancelled` or `skipped`; a check that rejects only
#        `failure` lets a CANCELLED run report success. `ci.yml` sets
#        `cancel-in-progress: true`, so that is reachable on any push-over-push,
#        and `verify-ci` would then green-light a release whose tests never ran.
#     2. The required SET silently narrows. A new job added to `ci.yml` is NOT
#        gated unless it is also listed in `ci-success`'s `needs:` — the `needs`
#        context contains only what is listed there. GitHub Actions cannot express
#        "require every job", so the invariant has to be asserted from outside the
#        workflow. This gate is that outside.
#
# What it asserts
#   A. `ci-success` exists, runs `if: always()`, and its `needs:` equals EVERY job
#      in ci.yml except itself and an explicit informational allow-list.
#   B. The allow-list is justified where it is claimed: `coverage` may be excluded
#      only while codecov.yml still marks coverage `informational: true`.
#   C. The gate step reads results from `${{ toJSON(needs) }}` (job-name-agnostic,
#      so adding a job to `needs:` needs no script edit).
#   D. BEHAVIOR, not shape: the shipped step body is extracted and EXECUTED here
#      against a synthetic `needs` context for every result value —
#      success | failure | cancelled | skipped | an unknown future value | a
#      missing key | an empty / malformed context. Only all-`success` may exit 0.
#   E. release-please.yml's `verify-ci` accepts exactly one conclusion (`success`)
#      from the `CI Success` job, and `upload-assets` still lists `verify-ci` in
#      `needs` so the gate is actually load-bearing.
#
#   Anything unparsable is a FAILURE, never a silent pass.
#
# Runs no Cargo, makes no network request, writes only inside a temp dir.
#
# Usage
#   scripts/check-ci-gate.sh [REPO_ROOT]
#   REPO_ROOT defaults to the repository root (the script's parent dir).

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
REPO_ROOT="${1:-"$(cd -- "$SCRIPT_DIR/.." && pwd -P)"}"

command -v python3 > /dev/null 2>&1 || {
	printf 'check-ci-gate: ERROR: python3 not found (needed to parse the workflow YAML)\n' >&2
	exit 2
}
command -v jq > /dev/null 2>&1 || {
	printf 'check-ci-gate: ERROR: jq not found (needed to run the gate body truth table)\n' >&2
	exit 2
}

WORK="$(mktemp -d)"
cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT

BODY="$WORK/ci-success-body.sh"
JOBS="$WORK/required-jobs.txt"

# --- Phase 1 — structural assertions (A, B, C, E) --------------------------
CG_CI_GATE_ROOT="$REPO_ROOT" CG_CI_GATE_BODY="$BODY" CG_CI_GATE_JOBS="$JOBS" python3 <<'PY'
import os
import re
import sys

ROOT = os.environ["CG_CI_GATE_ROOT"]
BODY_OUT = os.environ["CG_CI_GATE_BODY"]
JOBS_OUT = os.environ["CG_CI_GATE_JOBS"]

CI = os.path.join(ROOT, ".github", "workflows", "ci.yml")
RELEASE = os.path.join(ROOT, ".github", "workflows", "release-please.yml")
CODECOV = os.path.join(ROOT, "codecov.yml")

GATE_JOB = "ci-success"
GATE_JOB_NAME = "CI Success"
# Jobs allowed to stay OUT of the gate. Each entry must be justified below in
# `justify_exclusion`; an unjustifiable entry is a failure, not a pass.
INFORMATIONAL = {"coverage"}

failures = []


def fail(area, msg):
    failures.append((area, msg))


def die(msg, code=2):
    print("check-ci-gate: ERROR: %s" % msg, file=sys.stderr)
    sys.exit(code)


def load(path, label):
    try:
        with open(path, "r", encoding="utf-8") as fh:
            text = fh.read()
    except OSError as exc:
        die("cannot read %s (%s): %s" % (label, path, exc))
    try:
        data = yaml.safe_load(text)
    except yaml.YAMLError as exc:
        die("%s is not valid YAML: %s" % (label, exc))
    if not isinstance(data, dict):
        die("%s did not parse to a mapping" % label)
    return data


try:
    import yaml
except ImportError:
    die("PyYAML is required to parse the workflows (pip install PyYAML)")

ci = load(CI, "ci.yml")
release = load(RELEASE, "release-please.yml")
codecov = load(CODECOV, "codecov.yml")

ci_jobs = ci.get("jobs")
if not isinstance(ci_jobs, dict) or not ci_jobs:
    die("ci.yml has no jobs mapping")

gate = ci_jobs.get(GATE_JOB)
if not isinstance(gate, dict):
    die("ci.yml has no `%s` job (the required status check)" % GATE_JOB)
if gate.get("name") != GATE_JOB_NAME:
    fail(
        "gate-identity",
        "ci.yml `%s` is named %r, but branch protection and release-please.yml's "
        "verify-ci both look for %r" % (GATE_JOB, gate.get("name"), GATE_JOB_NAME),
    )

# --- A: `if: always()` -----------------------------------------------------
# Without it the gate is SKIPPED whenever a needed job fails, so it never turns
# red — it just never reports, and the strict check below would never run.
gate_if = gate.get("if")
if not isinstance(gate_if, str) or gate_if.strip() not in ("always()", "${{ always() }}"):
    fail(
        "gate-always",
        "`%s` has `if: %r`; it must be `always()` so the gate RUNS (and fails) "
        "when a required job did not succeed, instead of being skipped" % (GATE_JOB, gate_if),
    )

# --- A: the required set -------------------------------------------------
needs = gate.get("needs")
if isinstance(needs, str):
    needs = [needs]
if not isinstance(needs, list) or not needs:
    die("`%s` has no `needs:` list — it would gate nothing" % GATE_JOB)
needs = [str(n) for n in needs]

if len(set(needs)) != len(needs):
    fail("gate-needs", "`%s` `needs:` contains duplicates: %s" % (GATE_JOB, needs))
if GATE_JOB in needs:
    fail("gate-needs", "`%s` lists itself in `needs:`" % GATE_JOB)

expected = sorted(set(ci_jobs) - {GATE_JOB} - INFORMATIONAL)
actual = sorted(set(needs))

ungated = [j for j in expected if j not in actual]
unknown = [j for j in actual if j not in ci_jobs]
excluded_but_needed = [j for j in actual if j in INFORMATIONAL]

if ungated:
    fail(
        "gate-needs",
        "job(s) %s exist in ci.yml but are NOT in `%s`'s `needs:`, so their result "
        "cannot fail the gate. Add them to `needs:` (or, if genuinely "
        "informational, add them to INFORMATIONAL in scripts/check-ci-gate.sh "
        "WITH a written justification)." % (ungated, GATE_JOB),
    )
if unknown:
    fail(
        "gate-needs",
        "`%s` needs %s, which is not a job in ci.yml (a rename would make the "
        "workflow invalid and the gate meaningless)" % (GATE_JOB, unknown),
    )
if excluded_but_needed:
    fail(
        "gate-needs",
        "`%s` needs %s, which is on the informational allow-list — the two "
        "statements contradict each other" % (GATE_JOB, excluded_but_needed),
    )

# --- B: justify every exclusion -----------------------------------------
def justify_exclusion(job):
    """Return None when the exclusion is still justified, else the reason it is not."""
    if job not in ci_jobs:
        return "it is on the allow-list but no such job exists in ci.yml (stale entry)"
    if job == "coverage":
        # Coverage may be excluded ONLY while it is genuinely non-blocking: the
        # Codecov status must still be informational, otherwise a below-target %
        # would gate merges through a path this gate does not see.
        status = ((codecov.get("coverage") or {}).get("status") or {})
        if not isinstance(status, dict) or not status:
            return "codecov.yml has no coverage.status block to prove it is informational"
        for context, cfg in status.items():
            default = (cfg or {}).get("default") if isinstance(cfg, dict) else None
            if not isinstance(default, dict):
                return "codecov.yml coverage.status.%s has no `default` mapping" % context
            if default.get("informational") is not True:
                return (
                    "codecov.yml coverage.status.%s.default.informational is %r, not true — "
                    "coverage would gate, so it must not be excluded here"
                    % (context, default.get("informational"))
                )
        return None
    return "no justification is recorded in scripts/check-ci-gate.sh for excluding it"


for job in sorted(INFORMATIONAL):
    reason = justify_exclusion(job)
    if reason:
        fail("gate-exclusion", "excluding %r from the gate is not justified: %s" % (job, reason))

# --- C + D prep: the gate step ------------------------------------------
steps = gate.get("steps")
if not isinstance(steps, list) or not steps:
    die("`%s` has no steps" % GATE_JOB)

run_steps = [s for s in steps if isinstance(s, dict) and isinstance(s.get("run"), str)]
if len(run_steps) != 1:
    die(
        "`%s` has %d `run:` step(s); this gate understands exactly one "
        "(the strict status check)" % (GATE_JOB, len(run_steps))
    )
step = run_steps[0]
body = step["run"]

env = step.get("env") or {}
if not isinstance(env, dict):
    die("`%s`'s run step has a non-mapping `env:`" % GATE_JOB)
needs_json = env.get("NEEDS_JSON")
TOJSON_RE = re.compile(r"^\$\{\{\s*toJSON\(\s*needs\s*\)\s*\}\}$")
if not isinstance(needs_json, str) or not TOJSON_RE.match(needs_json.strip()):
    fail(
        "gate-shape",
        "the gate step must read every result from `env.NEEDS_JSON: ${{ toJSON(needs) }}` "
        "(job-name-agnostic, so adding a job to `needs:` needs no script edit); found %r"
        % (needs_json,),
    )

# A per-job `needs.<job>.result` expression in the body would re-introduce the
# hardcoded-name problem the toJSON form exists to remove.
hardcoded = re.findall(r"needs\.[A-Za-z0-9_-]+\.result", body)
if hardcoded:
    fail(
        "gate-shape",
        "the gate body interpolates per-job expressions %s; results must come from "
        "NEEDS_JSON so a newly-needed job is covered automatically" % (sorted(set(hardcoded)),)
    )

with open(BODY_OUT, "w", encoding="utf-8") as fh:
    fh.write(body if body.endswith("\n") else body + "\n")
with open(JOBS_OUT, "w", encoding="utf-8") as fh:
    fh.write("\n".join(actual) + "\n")

# --- E: the release-side gate -------------------------------------------
rel_jobs = release.get("jobs")
if not isinstance(rel_jobs, dict):
    die("release-please.yml has no jobs mapping")

verify = rel_jobs.get("verify-ci")
if not isinstance(verify, dict):
    die("release-please.yml has no `verify-ci` job (the release-side CI gate)")

vsteps = verify.get("steps")
if not isinstance(vsteps, list) or not vsteps:
    die("`verify-ci` has no steps")

wait_body = None
for s in vsteps:
    if isinstance(s, dict) and isinstance(s.get("run"), str) and "conclusion" in s["run"]:
        wait_body = s["run"]
        break
if wait_body is None:
    die("could not find `verify-ci`'s conclusion-polling step (nothing references `conclusion`)")

if GATE_JOB_NAME not in wait_body:
    fail(
        "release-gate",
        "`verify-ci` does not select the %r job; it would wait on the wrong thing" % GATE_JOB_NAME,
    )

# The ONLY accepted conclusion must be `success`: exactly one `exit 0` in the
# polling body, and it must be inside the `= "success"` branch.
success_test = re.search(r'if\s+\[\s+"\$conclusion"\s+=\s+"success"\s+\]\s*;\s*then', wait_body)
if not success_test:
    fail(
        "release-gate",
        "`verify-ci` has no strict `[ \"$conclusion\" = \"success\" ]` accept test; a "
        "cancelled/skipped CI run could satisfy the release gate",
    )

exit_zeros = [m.start() for m in re.finditer(r"^\s*exit 0\s*$", wait_body, re.M)]
if len(exit_zeros) != 1:
    fail(
        "release-gate",
        "`verify-ci`'s polling body has %d `exit 0` statement(s); exactly one is expected, "
        "guarded by the success test" % len(exit_zeros),
    )
elif success_test and not (success_test.end() < exit_zeros[0]):
    fail(
        "release-gate",
        "`verify-ci`'s `exit 0` is not inside the `= \"success\"` branch, so a non-success "
        "conclusion could pass the release gate",
    )

if not re.search(r"^\s*exit 1\s*$", wait_body, re.M):
    fail("release-gate", "`verify-ci`'s polling body never `exit 1`s; it cannot block a release")

upload = rel_jobs.get("upload-assets")
if not isinstance(upload, dict):
    die("release-please.yml has no `upload-assets` job")
up_needs = upload.get("needs")
if isinstance(up_needs, str):
    up_needs = [up_needs]
if not isinstance(up_needs, list) or "verify-ci" not in [str(n) for n in up_needs]:
    fail(
        "release-gate",
        "`upload-assets` does not list `verify-ci` in `needs:` (%r), so the CI gate would "
        "not block the first public-facing release step" % (up_needs,),
    )

if failures:
    for area, msg in failures:
        print("check-ci-gate: MISMATCH [%s]: %s" % (area, msg), file=sys.stderr)
    print(
        "check-ci-gate: FAIL: %d CI/release gate-integrity problem(s)" % len(failures),
        file=sys.stderr,
    )
    sys.exit(1)

print("check-ci-gate: structure OK")
print("  gate job           : %s (%r, if: %s)" % (GATE_JOB, GATE_JOB_NAME, gate_if))
print("  required jobs      : %s" % ", ".join(actual))
print("  excluded (informational, justified): %s" % (", ".join(sorted(INFORMATIONAL)) or "none"))
print("  release gate       : verify-ci waits on %r, accepts only 'success'" % GATE_JOB_NAME)
PY

# --- Phase 2 — behavioral truth table over the SHIPPED body (D) ------------
# The extracted body is executed with a synthetic `needs` context. This is a
# LOCAL evaluation of the gate expression; it proves the logic, not GitHub's
# runner behavior.
mapfile -t REQUIRED < "$JOBS"

# needs_json RESULT [OVERRIDE_JOB OVERRIDE_RESULT]
needs_json() {
	local base="$1" job="${2:-}" value="${3:-}"
	local out="{}" j
	for j in "${REQUIRED[@]}"; do
		local r="$base"
		if [ -n "$job" ] && [ "$j" = "$job" ]; then r="$value"; fi
		out="$(jq -cn --argjson acc "$out" --arg k "$j" --arg v "$r" '$acc + {($k): {result: $v}}')"
	done
	printf '%s' "$out"
}

TT_PASS=0
TT_FAIL=0
run_body() { # run_body NEEDS_JSON -> sets RC/OUT
	local payload="$1"
	OUT="$WORK/tt.out"
	set +e
	NEEDS_JSON="$payload" bash "$BODY" > "$OUT" 2>&1
	RC=$?
	set -e
}

expect() { # expect LABEL WANT(zero|nonzero) [MUST_MATCH_REGEX]
	local label="$1" want="$2" re="${3:-}"
	local bad=0
	if [ "$want" = "zero" ] && [ "$RC" -ne 0 ]; then bad=1; fi
	if [ "$want" = "nonzero" ] && [ "$RC" -eq 0 ]; then bad=1; fi
	if [ "$bad" -eq 0 ] && [ -n "$re" ] && ! grep -Eq -- "$re" "$OUT"; then bad=1; fi
	if [ "$bad" -eq 1 ]; then
		printf 'check-ci-gate: TRUTH-TABLE FAIL: %s (want %s, got exit %s)\n' "$label" "$want" "$RC" >&2
		sed 's/^/    /' "$OUT" >&2
		TT_FAIL=$((TT_FAIL + 1))
	else
		printf '  %-46s exit %-3s %s\n' "$label" "$RC" "OK"
		TT_PASS=$((TT_PASS + 1))
	fi
}

printf 'check-ci-gate: truth table over the shipped gate body (%d required job(s))\n' "${#REQUIRED[@]}"

run_body "$(needs_json success)"
expect "all jobs success" zero '✅ CI passed'

FIRST="${REQUIRED[0]}"
for value in failure cancelled skipped neutral timed_out unknown_future_value ""; do
	shown="${value:-<empty-string>}"
	run_body "$(needs_json success "$FIRST" "$value")"
	expect "$FIRST = ${shown}" nonzero "required job '${FIRST}' concluded"
done

# Every required job must be individually able to redden the gate.
for j in "${REQUIRED[@]}"; do
	run_body "$(needs_json success "$j" cancelled)"
	expect "$j = cancelled (per-job coverage)" nonzero "required job '${j}' concluded 'cancelled'"
done

# All-cancelled: every job must be reported, not just the first.
run_body "$(needs_json cancelled)"
expect "all jobs cancelled" nonzero "${#REQUIRED[@]} of ${#REQUIRED[@]} required job"

# Degenerate contexts must fail CLOSED.
run_body ""
expect "empty needs context" nonzero 'failing closed'
run_body "{}"
expect "empty JSON object" nonzero 'failing closed'
run_body "not json"
expect "malformed JSON" nonzero 'failing closed'
run_body '{"test":{}}'
expect "result key missing" nonzero '<missing>'

if [ "$TT_FAIL" -ne 0 ]; then
	printf 'check-ci-gate: FAIL: %d truth-table case(s) failed (%d passed)\n' "$TT_FAIL" "$TT_PASS" >&2
	exit 1
fi

printf 'check-ci-gate: OK (%d truth-table cases; only all-success exits 0)\n' "$TT_PASS"
