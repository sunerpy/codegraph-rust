#!/usr/bin/env bash
# ci-gate.test.sh — fixture harness for the CI/release gate-integrity guard.
#
# Copies the three REAL files the guard parses
#   .github/workflows/ci.yml, .github/workflows/release-please.yml, codecov.yml
# into a throwaway repo-shaped fixture, mutates ONE of them per scenario, and runs
# scripts/check-ci-gate.sh against that fixture root. Nothing under the real
# repository is touched, no Cargo runs, no network request is made, and no GitHub
# Actions run is triggered — every claim here is a LOCAL evaluation of the gate
# expression plus a YAML parse.
#
# It proves:
#   * pristine copy of the real files              -> exit 0,
#   * the real repository root                     -> exit 0,
#   * cosmetic YAML churn (comment + requoting)    -> STILL exit 0 (no false alarm),
#   * NEGATIVE CONTROL: the pre-fix `== "failure"` expression admits `cancelled`
#     and `skipped`, while the shipped strict body rejects both,
#   * ci.yml reverted to the `== "failure"` shape   -> red, blames gate-shape,
#   * a required job dropped from `needs:`         -> red, names the ungated job,
#   * `if: always()` removed from the gate         -> red, blames gate-always,
#   * a new job added but not gated                -> red, names it,
#   * codecov.yml flipped to informational: false  -> red, blames gate-exclusion,
#   * verify-ci relaxed to reject only `failure`   -> red, blames release-gate,
#   * upload-assets dropping verify-ci from needs  -> red, blames release-gate,
#   * an unparsable workflow                       -> red (fails CLOSED).
#
# Usage: scripts/tests/ci-gate.test.sh

set -euo pipefail

HARNESS_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
REPO_ROOT="$(cd -- "$HARNESS_DIR/../.." && pwd -P)"
GATE="$REPO_ROOT/scripts/check-ci-gate.sh"

CI_REL=".github/workflows/ci.yml"
REL_REL=".github/workflows/release-please.yml"
COV_REL="codecov.yml"

for f in "$GATE" "$REPO_ROOT/$CI_REL" "$REPO_ROOT/$REL_REL" "$REPO_ROOT/$COV_REL"; do
	[ -f "$f" ] || {
		printf 'harness: required file not found: %s\n' "$f" >&2
		exit 2
	}
done

WORK="$(mktemp -d)"
cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT

PASS=0
FAIL=0
note() { printf '  %s\n' "$1"; }
ok() {
	printf 'PASS: %s\n' "$1"
	PASS=$((PASS + 1))
}
bad() {
	printf 'FAIL: %s\n' "$1" >&2
	FAIL=$((FAIL + 1))
}

make_fixture() {
	local name="$1"
	local root="$WORK/$name"
	mkdir -p "$root/.github/workflows"
	cp "$REPO_ROOT/$CI_REL" "$root/$CI_REL"
	cp "$REPO_ROOT/$REL_REL" "$root/$REL_REL"
	cp "$REPO_ROOT/$COV_REL" "$root/$COV_REL"
	printf '%s' "$root"
}

RC=0
OUT=""
ERR=""
run_gate() {
	local name="$1" root="$2"
	OUT="$WORK/$name.out"
	ERR="$WORK/$name.err"
	set +e
	bash "$GATE" "$root" > "$OUT" 2> "$ERR"
	RC=$?
	set -e
}

assert_rc() {
	local name="$1" want="$2"
	if [ "$RC" != "$want" ]; then
		bad "$name: expected exit $want, got $RC"
		note "stdout: $(tr '\n' '|' < "$OUT")"
		note "stderr: $(tr '\n' '|' < "$ERR")"
		return 1
	fi
}
assert_rc_nonzero() {
	local name="$1"
	if [ "$RC" -eq 0 ]; then
		bad "$name: expected a NONZERO exit, got 0"
		note "stdout: $(tr '\n' '|' < "$OUT")"
		return 1
	fi
}
assert_stdout() {
	local name="$1" re="$2"
	if ! grep -Eq -- "$re" "$OUT"; then
		bad "$name: expected /$re/ on stdout"
		note "stdout: $(tr '\n' '|' < "$OUT")"
		return 1
	fi
}
assert_stderr() {
	local name="$1" re="$2"
	if ! grep -Eq -- "$re" "$ERR"; then
		bad "$name: expected diagnostic /$re/ on stderr"
		note "stderr: $(tr '\n' '|' < "$ERR")"
		return 1
	fi
}

mutate() {
	local file="$1" old="$2" new="$3"
	CG_MUT_FILE="$file" CG_MUT_OLD="$old" CG_MUT_NEW="$new" python3 - <<'PY'
import os
import sys

path = os.environ["CG_MUT_FILE"]
old = os.environ["CG_MUT_OLD"]
new = os.environ["CG_MUT_NEW"]
with open(path, "r", encoding="utf-8") as fh:
    text = fh.read()
if old not in text:
    print("harness: mutation anchor not found in %s: %r" % (path, old), file=sys.stderr)
    sys.exit(2)
with open(path, "w", encoding="utf-8") as fh:
    fh.write(text.replace(old, new, 1))
PY
}

printf '=== CI gate-integrity fixture harness ===\n'

# --- Scenario A — pristine copies of the real files: green. ----------------
A="$(make_fixture A_pristine)"
a_ok=1
run_gate "A_pristine" "$A"
assert_rc "A_pristine" 0 || a_ok=0
assert_stdout "A_pristine" 'check-ci-gate: structure OK' || a_ok=0
assert_stdout "A_pristine" 'required jobs      : audit, test, windows' || a_ok=0
assert_stdout "A_pristine" 'check-ci-gate: OK \([0-9]+ truth-table cases' || a_ok=0
[ "$a_ok" -eq 1 ] && ok "A_pristine (exit=0, gate strict + truth table green)"

# --- Scenario B — the real repository root: green. -------------------------
b_ok=1
run_gate "B_repository" "$REPO_ROOT"
assert_rc "B_repository" 0 || b_ok=0
assert_stdout "B_repository" 'check-ci-gate: OK' || b_ok=0
[ "$b_ok" -eq 1 ] && ok "B_repository (exit=0, the shipped tree is strictly gated)"

# --- Scenario C — cosmetic churn must NOT trip the guard. ------------------
C="$(make_fixture C_cosmetic)"
mutate "$C/$CI_REL" '    needs: [test, audit, windows]' \
	'    # cosmetic: reflowed list, same three jobs
    needs:
      - test
      - audit
      - windows'
c_ok=1
run_gate "C_cosmetic" "$C"
assert_rc "C_cosmetic" 0 || c_ok=0
assert_stdout "C_cosmetic" 'required jobs      : audit, test, windows' || c_ok=0
[ "$c_ok" -eq 1 ] && ok "C_cosmetic (exit=0, YAML reflow is not a gate change)"

# --- Scenario D — NEGATIVE CONTROL: old vs new expression truth table. -----
# The pre-fix gate body, verbatim, with GitHub's `${{ needs.X.result }}`
# interpolation performed here the way the runner performs it (textual
# substitution before bash sees the script).
LEGACY_TMPL='if [[ "@TEST@" == "failure" || "@AUDIT@" == "failure" || "@WINDOWS@" == "failure" ]]; then
  echo "❌ CI failed"
  exit 1
fi
echo "✅ CI passed"'

legacy_eval() { # legacy_eval RESULT -> exit status of the OLD gate
	local r="$1" script
	script="${LEGACY_TMPL//@TEST@/$r}"
	script="${script//@AUDIT@/$r}"
	script="${script//@WINDOWS@/$r}"
	set +e
	printf '%s\n' "$script" | bash > /dev/null 2>&1
	local rc=$?
	set -e
	printf '%s' "$rc"
}

# Extract the SHIPPED strict body straight out of the real ci.yml and evaluate it
# over the same four values, so both columns come from real files.
STRICT_BODY="$WORK/strict-body.sh"
CG_CI="$REPO_ROOT/$CI_REL" CG_OUT="$STRICT_BODY" python3 - <<'PY'
import os
import sys

import yaml

with open(os.environ["CG_CI"], "r", encoding="utf-8") as fh:
    ci = yaml.safe_load(fh)
steps = ci["jobs"]["ci-success"]["steps"]
bodies = [s["run"] for s in steps if isinstance(s, dict) and isinstance(s.get("run"), str)]
if len(bodies) != 1:
    print("harness: expected exactly one run step in ci-success", file=sys.stderr)
    sys.exit(2)
with open(os.environ["CG_OUT"], "w", encoding="utf-8") as fh:
    fh.write(bodies[0])
PY

strict_eval() { # strict_eval RESULT -> exit status of the NEW gate
	local r="$1" payload
	payload="$(jq -cn --arg v "$r" '{test:{result:$v},audit:{result:$v},windows:{result:$v}}')"
	set +e
	NEEDS_JSON="$payload" bash "$STRICT_BODY" > /dev/null 2>&1
	local rc=$?
	set -e
	printf '%s' "$rc"
}

printf '  truth table — result | OLD exit | NEW exit | OLD verdict | NEW verdict\n'
d_ok=1
for value in success failure cancelled skipped; do
	old_rc="$(legacy_eval "$value")"
	new_rc="$(strict_eval "$value")"
	old_verdict=$([ "$old_rc" -eq 0 ] && echo ADMITS || echo rejects)
	new_verdict=$([ "$new_rc" -eq 0 ] && echo ADMITS || echo rejects)
	printf '    %-10s |    %-5s |    %-5s | %-7s | %s\n' \
		"$value" "$old_rc" "$new_rc" "$old_verdict" "$new_verdict"
	case "$value" in
		success)
			{ [ "$old_rc" -eq 0 ] && [ "$new_rc" -eq 0 ]; } || d_ok=0
			;;
		failure)
			{ [ "$old_rc" -ne 0 ] && [ "$new_rc" -ne 0 ]; } || d_ok=0
			;;
		cancelled | skipped)
			# THE DEFECT: old admits, new rejects.
			{ [ "$old_rc" -eq 0 ] && [ "$new_rc" -ne 0 ]; } || d_ok=0
			;;
	esac
done
[ "$d_ok" -eq 1 ] \
	&& ok "D_negative_control (old ADMITS cancelled+skipped; new rejects them, both agree on success/failure)" \
	|| bad "D_negative_control: the old/new truth table did not match the documented defect"

# --- Scenario E — MUTANT: ci.yml reverted to the pre-fix expression. -------
E="$(make_fixture E_legacy_expression)"
mutate "$E/$CI_REL" '        env:
          # The whole needs context as JSON: {"test":{"result":"success",...},...}
          NEEDS_JSON: ${{ toJSON(needs) }}
' ''
mutate "$E/$CI_REL" '        run: |
          set -euo pipefail
' '        run: |
          if [[ "${{ needs.test.result }}" == "failure" ]]; then exit 1; fi
          echo "legacy"
          set -euo pipefail
'
e_ok=1
run_gate "E_legacy_expression" "$E"
assert_rc_nonzero "E_legacy_expression" || e_ok=0
assert_stderr "E_legacy_expression" 'MISMATCH \[gate-shape\]' || e_ok=0
assert_stderr "E_legacy_expression" 'needs\.test\.result' || e_ok=0
[ "$e_ok" -eq 1 ] && ok "E_legacy_expression (exit=$RC, per-job interpolation is rejected)"

# --- Scenario F — MUTANT: a required job dropped from `needs:`. ------------
F="$(make_fixture F_job_ungated)"
mutate "$F/$CI_REL" '    needs: [test, audit, windows]' '    needs: [test, audit]'
f_ok=1
run_gate "F_job_ungated" "$F"
assert_rc_nonzero "F_job_ungated" || f_ok=0
assert_stderr "F_job_ungated" 'MISMATCH \[gate-needs\]' || f_ok=0
assert_stderr "F_job_ungated" "\\['windows'\\] exist in ci\\.yml but are NOT in" || f_ok=0
[ "$f_ok" -eq 1 ] && ok "F_job_ungated (exit=$RC, names the ungated job)"

# --- Scenario G — MUTANT: `if: always()` removed from the gate. ------------
G="$(make_fixture G_no_always)"
mutate "$G/$CI_REL" '    runs-on: ubuntu-latest
    if: always()' '    runs-on: ubuntu-latest'
g_ok=1
run_gate "G_no_always" "$G"
assert_rc_nonzero "G_no_always" || g_ok=0
assert_stderr "G_no_always" 'MISMATCH \[gate-always\]' || g_ok=0
[ "$g_ok" -eq 1 ] && ok "G_no_always (exit=$RC, a skippable gate is not a gate)"

# --- Scenario H — MUTANT: a NEW job appears but is never gated. ------------
# This is the "robust to a job added later" case: GitHub Actions cannot express
# it inside the workflow, so the guard must catch it from outside — loudly.
H="$(make_fixture H_new_job_ungated)"
mutate "$H/$CI_REL" '  ci-success:' '  fuzz:
    name: Fuzz
    runs-on: ubuntu-latest
    steps:
      - run: echo fuzzing

  ci-success:'
h_ok=1
run_gate "H_new_job_ungated" "$H"
assert_rc_nonzero "H_new_job_ungated" || h_ok=0
assert_stderr "H_new_job_ungated" 'MISMATCH \[gate-needs\]' || h_ok=0
assert_stderr "H_new_job_ungated" "\\['fuzz'\\]" || h_ok=0
[ "$h_ok" -eq 1 ] && ok "H_new_job_ungated (exit=$RC, a later-added job cannot slip past the gate)"

# --- Scenario I — MUTANT: coverage stops being informational. --------------
I="$(make_fixture I_coverage_blocking)"
mutate "$I/$COV_REL" '        informational: true # report-only' '        informational: false # report-only'
i_ok=1
run_gate "I_coverage_blocking" "$I"
assert_rc_nonzero "I_coverage_blocking" || i_ok=0
assert_stderr "I_coverage_blocking" 'MISMATCH \[gate-exclusion\]' || i_ok=0
assert_stderr "I_coverage_blocking" 'informational is False, not true' || i_ok=0
[ "$i_ok" -eq 1 ] && ok "I_coverage_blocking (exit=$RC, the exclusion cannot outlive its justification)"

# --- Scenario J — MUTANT: verify-ci relaxed to reject only `failure`. ------
J="$(make_fixture J_release_gate_relaxed)"
mutate "$J/$REL_REL" 'if [ "$conclusion" = "success" ]; then' 'if [ "$conclusion" != "failure" ]; then'
j_ok=1
run_gate "J_release_gate_relaxed" "$J"
assert_rc_nonzero "J_release_gate_relaxed" || j_ok=0
assert_stderr "J_release_gate_relaxed" 'MISMATCH \[release-gate\]' || j_ok=0
assert_stderr "J_release_gate_relaxed" 'cancelled/skipped CI run could satisfy the release gate' || j_ok=0
[ "$j_ok" -eq 1 ] && ok "J_release_gate_relaxed (exit=$RC, the release-side gate must be strict too)"

# --- Scenario K — MUTANT: upload-assets stops needing verify-ci. -----------
K="$(make_fixture K_gate_not_wired)"
mutate "$K/$REL_REL" '    needs: [release-please, build-binaries, verify-ci]' \
	'    needs: [release-please, build-binaries]'
k_ok=1
run_gate "K_gate_not_wired" "$K"
assert_rc_nonzero "K_gate_not_wired" || k_ok=0
assert_stderr "K_gate_not_wired" 'MISMATCH \[release-gate\]' || k_ok=0
assert_stderr "K_gate_not_wired" 'does not list `verify-ci` in `needs:`' || k_ok=0
[ "$k_ok" -eq 1 ] && ok "K_gate_not_wired (exit=$RC, an unwired gate is reported)"

# --- Scenario L — an unparsable workflow must FAIL CLOSED. ----------------
L="$(make_fixture L_unparsable)"
mutate "$L/$CI_REL" 'jobs:' 'jobs: [this is not a mapping]
ignored:'
l_ok=1
run_gate "L_unparsable" "$L"
assert_rc_nonzero "L_unparsable" || l_ok=0
assert_stderr "L_unparsable" 'check-ci-gate: ERROR' || l_ok=0
[ "$l_ok" -eq 1 ] && ok "L_unparsable (exit=$RC, an unreadable gate is a failure, not a pass)"

printf '=== harness result: %d passed, %d failed ===\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ]
