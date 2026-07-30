#!/usr/bin/env bash
# asset-name-drift.test.sh — fixture harness for the asset-name drift gate.
#
# Copies the three REAL files the gate parses
#   .github/workflows/release-please.yml, scripts/install.sh, scripts/install.ps1
# into a throwaway repo-shaped fixture, mutates ONE of them per scenario, and runs
# scripts/check-asset-names.sh against that fixture root. Nothing under the real
# repository is touched, no Cargo runs, no network request is made.
#
# It proves the gate:
#   * pristine copy of the real files            -> exit 0,
#   * the real repository root                   -> exit 0,
#   * cosmetic YAML churn (comment + requoting)  -> STILL exit 0 (no false alarm),
#   * workflow tar.gz name separator changed     -> red, names workflow(tar.gz),
#   * install.sh BIN renamed                     -> red, names binary-name,
#   * upload-assets download `pattern:` changed  -> red, names artifact-plumbing,
#   * install.ps1 asset field order swapped      -> red, names install.ps1,
#   * install.ps1 loses ARM64 detection          -> red, names the uncovered target,
#   * SHA256SUMS dropped from release `files:`   -> red, names release-files,
#   * an unparsable workflow                     -> red (fails CLOSED, never passes).
#
# Usage: scripts/tests/asset-name-drift.test.sh

set -euo pipefail

HARNESS_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
REPO_ROOT="$(cd -- "$HARNESS_DIR/../.." && pwd -P)"
GATE="$REPO_ROOT/scripts/check-asset-names.sh"

WORKFLOW_REL=".github/workflows/release-please.yml"
SH_REL="scripts/install.sh"
PS_REL="scripts/install.ps1"

for f in "$GATE" "$REPO_ROOT/$WORKFLOW_REL" "$REPO_ROOT/$SH_REL" "$REPO_ROOT/$PS_REL"; do
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

# make_fixture NAME -> echoes a fresh fixture root holding verbatim copies.
make_fixture() {
	local name="$1"
	local root="$WORK/$name"
	mkdir -p "$root/.github/workflows" "$root/scripts"
	cp "$REPO_ROOT/$WORKFLOW_REL" "$root/$WORKFLOW_REL"
	cp "$REPO_ROOT/$SH_REL" "$root/$SH_REL"
	cp "$REPO_ROOT/$PS_REL" "$root/$PS_REL"
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
	if ! grep -Eq "$re" "$OUT"; then
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
assert_not_stderr() {
	local name="$1" re="$2"
	if grep -Eq -- "$re" "$ERR"; then
		bad "$name: did NOT expect /$re/ on stderr (over-broad diagnosis)"
		note "stderr: $(tr '\n' '|' < "$ERR")"
		return 1
	fi
}

# Replace the FIRST literal occurrence of OLD with NEW in FILE; fail loudly when
# OLD is absent, so a refactor that moves the anchor cannot silently neuter a
# mutant into a no-op.
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

printf '=== asset-name drift-gate fixture harness ===\n'

# --- Scenario A — pristine copies of the real files: green. ----------------
A="$(make_fixture A_pristine)"
a_ok=1
run_gate "A_pristine" "$A"
assert_rc "A_pristine" 0 || a_ok=0
assert_stdout "A_pristine" 'check-asset-names: OK' || a_ok=0
assert_stdout "A_pristine" 'matrix targets     : 6' || a_ok=0
for t in x86_64-unknown-linux-musl aarch64-unknown-linux-musl x86_64-apple-darwin \
	aarch64-apple-darwin x86_64-pc-windows-msvc aarch64-pc-windows-msvc; do
	assert_stdout "A_pristine" "$t" || a_ok=0
done
[ "$a_ok" -eq 1 ] && ok "A_pristine (exit=0, all six matrix targets owned by an installer)"

# --- Scenario B — the real repository root: green. -------------------------
b_ok=1
run_gate "B_repository" "$REPO_ROOT"
assert_rc "B_repository" 0 || b_ok=0
assert_stdout "B_repository" 'check-asset-names: OK' || b_ok=0
[ "$b_ok" -eq 1 ] && ok "B_repository (exit=0, the shipped tree agrees three ways)"

# --- Scenario C — cosmetic churn must NOT trip the gate. -------------------
C="$(make_fixture C_cosmetic)"
mutate "$C/$WORKFLOW_REL" '          path: dist/*' \
	'          # cosmetic: requoted + commented, same value
          path: "dist/*"'
mutate "$C/$WORKFLOW_REL" '          pattern: dist-*' '          pattern: "dist-*"'
c_ok=1
run_gate "C_cosmetic" "$C"
assert_rc "C_cosmetic" 0 || c_ok=0
assert_stdout "C_cosmetic" 'check-asset-names: OK' || c_ok=0
[ "$c_ok" -eq 1 ] && ok "C_cosmetic (exit=0, comments/quoting are not drift)"

# --- Scenario D — MUTANT: workflow tar.gz separator drifts. ----------------
D="$(make_fixture D_workflow_tar_name)"
mutate "$D/$WORKFLOW_REL" 'tar -czf "dist/${BINARY_NAME}-${{' 'tar -czf "dist/${BINARY_NAME}_${{'
d_ok=1
run_gate "D_workflow_tar_name" "$D"
assert_rc_nonzero "D_workflow_tar_name" || d_ok=0
assert_stderr "D_workflow_tar_name" 'MISMATCH \[workflow\(tar\.gz\)\]' || d_ok=0
assert_stderr "D_workflow_tar_name" '<bin>_<version>-<target>\.<ext>' || d_ok=0
assert_not_stderr "D_workflow_tar_name" 'MISMATCH \[install\.(sh|ps1)\]' || d_ok=0
[ "$d_ok" -eq 1 ] && ok "D_workflow_tar_name (exit=$RC, blames workflow(tar.gz) only)"

# --- Scenario E — MUTANT: install.sh binary name drifts. ------------------
E="$(make_fixture E_install_sh_bin)"
mutate "$E/$SH_REL" 'BIN="codegraph"' 'BIN="codegraf"'
e_ok=1
run_gate "E_install_sh_bin" "$E"
assert_rc_nonzero "E_install_sh_bin" || e_ok=0
assert_stderr "E_install_sh_bin" 'MISMATCH \[binary-name\]' || e_ok=0
assert_stderr "E_install_sh_bin" "install\\.sh BIN='codegraf'" || e_ok=0
assert_not_stderr "E_install_sh_bin" 'MISMATCH \[artifact-plumbing\]' || e_ok=0
[ "$e_ok" -eq 1 ] && ok "E_install_sh_bin (exit=$RC, blames binary-name and prints both values)"

# --- Scenario F — MUTANT: upload-assets download pattern drifts. -----------
F="$(make_fixture F_download_pattern)"
mutate "$F/$WORKFLOW_REL" 'pattern: dist-*' 'pattern: bins-*'
f_ok=1
run_gate "F_download_pattern" "$F"
assert_rc_nonzero "F_download_pattern" || f_ok=0
assert_stderr "F_download_pattern" 'MISMATCH \[artifact-plumbing\]' || f_ok=0
assert_stderr "F_download_pattern" "name 'dist-x86_64-unknown-linux-musl' does not match upload-assets pattern 'bins-\*'" || f_ok=0
assert_not_stderr "F_download_pattern" 'MISMATCH \[binary-name\]' || f_ok=0
[ "$f_ok" -eq 1 ] && ok "F_download_pattern (exit=$RC, blames artifact-plumbing with both strings)"

# --- Scenario G — MUTANT: install.ps1 asset field order swapped. -----------
G="$(make_fixture G_ps1_field_order)"
mutate "$G/$PS_REL" '$asset = "$Bin-$version-$target.$ext"' '$asset = "$Bin-$target-$version.$ext"'
g_ok=1
run_gate "G_ps1_field_order" "$G"
assert_rc_nonzero "G_ps1_field_order" || g_ok=0
assert_stderr "G_ps1_field_order" 'MISMATCH \[install\.ps1\]' || g_ok=0
assert_stderr "G_ps1_field_order" '<bin>-<target>-<version>\.<ext>' || g_ok=0
assert_not_stderr "G_ps1_field_order" 'MISMATCH \[workflow' || g_ok=0
[ "$g_ok" -eq 1 ] && ok "G_ps1_field_order (exit=$RC, blames install.ps1 only)"

# --- Scenario H — MUTANT: install.ps1 loses ARM64 detection. ---------------
H="$(make_fixture H_ps1_drops_arm64)"
mutate "$H/$PS_REL" "    '^(ARM64|aarch64)\$'    { \$archPart = 'aarch64' }
" ""
h_ok=1
run_gate "H_ps1_drops_arm64" "$H"
assert_rc_nonzero "H_ps1_drops_arm64" || h_ok=0
assert_stderr "H_ps1_drops_arm64" 'MISMATCH \[target-coverage\]' || h_ok=0
assert_stderr "H_ps1_drops_arm64" "matrix target 'aarch64-pc-windows-msvc' cannot be produced" || h_ok=0
[ "$h_ok" -eq 1 ] && ok "H_ps1_drops_arm64 (exit=$RC, names the orphaned matrix target)"

# --- Scenario I — MUTANT: SHA256SUMS dropped from the release files. -------
I="$(make_fixture I_sums_unpublished)"
mutate "$I/$WORKFLOW_REL" '            dist/SHA256SUMS
' ""
i_ok=1
run_gate "I_sums_unpublished" "$I"
assert_rc_nonzero "I_sums_unpublished" || i_ok=0
assert_stderr "I_sums_unpublished" 'MISMATCH \[release-files\]' || i_ok=0
assert_stderr "I_sums_unpublished" "installers fetch 'SHA256SUMS' but no release" || i_ok=0
[ "$i_ok" -eq 1 ] && ok "I_sums_unpublished (exit=$RC, an unpublished sums file is drift too)"

# --- Scenario J — an unparsable workflow must FAIL CLOSED. -----------------
J="$(make_fixture J_unparsable)"
mutate "$J/$WORKFLOW_REL" '      - name: Package (unix tar.gz)' '      - name: Package (unix TARBALL)'
mutate "$J/$WORKFLOW_REL" 'tar -czf "dist/${BINARY_NAME}' 'bsdtar --create --gzip --file "dist/${BINARY_NAME}'
j_ok=1
run_gate "J_unparsable" "$J"
assert_rc_nonzero "J_unparsable" || j_ok=0
assert_stderr "J_unparsable" 'check-asset-names: ERROR' || j_ok=0
[ "$j_ok" -eq 1 ] && ok "J_unparsable (exit=$RC, an unreadable contract is a failure, not a pass)"

printf '=== harness result: %d passed, %d failed ===\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ]
