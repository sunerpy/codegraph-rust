#!/usr/bin/env bash
# legacy-fixture-path-domain.test.sh — fixture harness for the legacy-fixture
# path-domain translation.
#
# WHY this harness exists: scripts/setup-legacy-fixture.sh runs under MSYS bash on
# the Windows CI runner, but the path it prints is consumed by a NATIVE Win32
# process (`cargo test` reading CODEGRAPH_LEGACY_BIN). An MSYS absolute path
# (`/tmp/...`) is a virtual namespace Win32 cannot resolve, so the emitted path
# must be translated. That translation only ever executes on Windows, which means
# a Linux/macOS test run can never observe it — it is exactly the kind of code
# that rots silently. Here it is driven directly with a stubbed `uname` and a
# stubbed `cygpath`, so the Windows branch is exercised on ANY host.
#
# The function under test is EXTRACTED VERBATIM from the shipped script (never
# re-implemented here), so the harness cannot drift from the real emitter.
#
# It proves:
#   * a non-Windows `uname` prints the path byte-for-byte, with no cygpath call,
#   * an MSYS/MINGW/CYGWIN/Windows_NT `uname` prints cygpath's native path,
#   * cygpath is invoked with `-m` (forward slashes: no backslash escaping when
#     the path crosses $GITHUB_ENV and Rust string handling),
#   * a missing cygpath on a Windows host FAILS LOUDLY instead of emitting an
#     untranslated MSYS path the consumer could not open,
#   * a cygpath that exits nonzero fails,
#   * a cygpath that prints nothing fails,
#   * the shipped script has NO remaining emission path that prints the raw
#     internal `$EXE` (both the cache-hit and the fresh-download exit route
#     through the translator).
#
# Nothing under the real repository is touched, no network request is made, and
# no real fixture is downloaded.
#
# Usage: scripts/tests/legacy-fixture-path-domain.test.sh

set -euo pipefail

HARNESS_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
REPO_ROOT="$(cd -- "$HARNESS_DIR/../.." && pwd -P)"
SCRIPT="$REPO_ROOT/scripts/setup-legacy-fixture.sh"

[ -f "$SCRIPT" ] || {
	printf 'harness: script not found: %s\n' "$SCRIPT" >&2
	exit 2
}

WORK="$(mktemp -d)"
cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT

PASS=0
FAIL=0
ok() {
	printf 'PASS: %s\n' "$1"
	PASS=$((PASS + 1))
}
bad() {
	printf 'FAIL: %s\n' "$1" >&2
	FAIL=$((FAIL + 1))
}

# The emitter, lifted verbatim from the shipped script.
FN="$WORK/emit.sh"
awk '/^emit_consumer_path\(\) \{/,/^\}/' "$SCRIPT" > "$FN"
grep -q '^emit_consumer_path() {' "$FN" || {
	printf 'harness: could not extract emit_consumer_path from %s\n' "$SCRIPT" >&2
	exit 2
}
grep -q '^}$' "$FN" || {
	printf 'harness: extracted emit_consumer_path is not terminated\n' >&2
	exit 2
}

# Driver: same `set -euo pipefail` and same `die` shape as the real script, so a
# `die` inside the function terminates the process exactly as it would in situ.
DRIVER="$WORK/driver.sh"
cat > "$DRIVER" << 'DRV'
set -euo pipefail
die() {
	printf 'setup-legacy-fixture: %s\n' "$1" >&2
	exit 1
}
. "$CG_FN"
emit_consumer_path "$CG_INPUT"
DRV

# stub_bin DIR UNAME_S [CYGPATH_BODY]
#   Builds a sandbox PATH whose `uname` reports UNAME_S. `cygpath` is created only
#   when CYGPATH_BODY is given, so "cygpath absent" is a genuine absence.
stub_bin() {
	local dir="$1" uname_s="$2" cygpath_body="${3-}"
	mkdir -p "$dir"
	printf '#!/bin/sh\nprintf "%%s\\n" "%s"\n' "$uname_s" > "$dir/uname"
	chmod +x "$dir/uname"
	if [ -n "$cygpath_body" ]; then
		printf '#!/bin/sh\n%s\n' "$cygpath_body" > "$dir/cygpath"
		chmod +x "$dir/cygpath"
	fi
}

# run_case LABEL UNAME_S INPUT [CYGPATH_BODY] -> RC, OUT, ERR
RC=0
OUT=""
ERR=""
run_case() {
	local label="$1" uname_s="$2" input="$3" cygpath_body="${4-}"
	local dir="$WORK/$label"
	rm -rf "$dir"
	stub_bin "$dir/bin" "$uname_s" "$cygpath_body"
	set +e
	OUT="$(
		PATH="$dir/bin:/usr/bin:/bin" CG_FN="$FN" CG_INPUT="$input" \
			bash "$DRIVER" 2> "$dir/err"
	)"
	RC=$?
	set -e
	ERR="$(cat "$dir/err")"
}

MSYS_PATH='/tmp/codegraph-legacy-fixture/e52703f3/codegraph.exe'
NATIVE_PATH='C:/Users/runneradmin/AppData/Local/Temp/codegraph-legacy-fixture/e52703f3/codegraph.exe'
# Records its argv so the harness can assert the `-m` flag, then translates.
CYGPATH_OK='printf "%s\n" "$*" > "$CG_ARGV_LOG"
printf "C:/Users/runneradmin/AppData/Local/Temp%s\n" "${3#/tmp}"'

# --- Scenario A — Linux host: byte-identical passthrough, no cygpath. --------
a_ok=1
run_case A_linux Linux '/tmp/codegraph-legacy-fixture/1a14d195/codegraph'
[ "$RC" -eq 0 ] || {
	bad "A_linux: expected exit 0, got $RC ($ERR)"
	a_ok=0
}
[ "$OUT" = '/tmp/codegraph-legacy-fixture/1a14d195/codegraph' ] || {
	bad "A_linux: path was rewritten: $OUT"
	a_ok=0
}
[ -z "$ERR" ] || {
	bad "A_linux: unexpected stderr: $ERR"
	a_ok=0
}
[ "$a_ok" -eq 1 ] && ok "A_linux (exit=0, path emitted verbatim, cygpath never consulted)"

# --- Scenario B — Darwin host: byte-identical passthrough. -------------------
b_ok=1
run_case B_darwin Darwin '/Users/runner/tmp/fixture/codegraph'
[ "$RC" -eq 0 ] || {
	bad "B_darwin: expected exit 0, got $RC ($ERR)"
	b_ok=0
}
[ "$OUT" = '/Users/runner/tmp/fixture/codegraph' ] || {
	bad "B_darwin: path was rewritten: $OUT"
	b_ok=0
}
[ "$b_ok" -eq 1 ] && ok "B_darwin (exit=0, path emitted verbatim)"

# --- Scenario C — every Windows-shell uname: translated to a native path. ----
for uname_s in MINGW64_NT-10.0-20348 MSYS_NT-10.0-20348 CYGWIN_NT-10.0 Windows_NT; do
	c_ok=1
	CG_ARGV_LOG="$WORK/argv_$uname_s.txt"
	export CG_ARGV_LOG
	run_case "C_$uname_s" "$uname_s" "$MSYS_PATH" "$CYGPATH_OK"
	[ "$RC" -eq 0 ] || {
		bad "C_$uname_s: expected exit 0, got $RC ($ERR)"
		c_ok=0
	}
	[ "$OUT" = "$NATIVE_PATH" ] || {
		bad "C_$uname_s: expected native path, got: $OUT"
		c_ok=0
	}
	case "$OUT" in
	*'\'*)
		bad "C_$uname_s: emitted path contains a backslash: $OUT"
		c_ok=0
		;;
	esac
	grep -q -- '-m' "$CG_ARGV_LOG" || {
		bad "C_$uname_s: cygpath was not invoked with -m (argv: $(cat "$CG_ARGV_LOG"))"
		c_ok=0
	}
	unset CG_ARGV_LOG
	[ "$c_ok" -eq 1 ] && ok "C_$uname_s (exit=0, MSYS path translated via 'cygpath -m', forward slashes)"
done

# --- Scenario D — Windows host, cygpath ABSENT: loud failure, no output. -----
d_ok=1
run_case D_no_cygpath MINGW64_NT-10.0-20348 "$MSYS_PATH"
[ "$RC" -ne 0 ] || {
	bad "D_no_cygpath: expected nonzero exit, got 0"
	d_ok=0
}
[ -z "$OUT" ] || {
	bad "D_no_cygpath: an untranslated path was emitted anyway: $OUT"
	d_ok=0
}
printf '%s' "$ERR" | grep -q 'cygpath not found on PATH' || {
	bad "D_no_cygpath: stderr did not name the missing tool: $ERR"
	d_ok=0
}
[ "$d_ok" -eq 1 ] && ok "D_no_cygpath (exit=$RC, refuses to emit an MSYS path Win32 cannot open)"

# --- Scenario E — cygpath exits nonzero: failure, no output. -----------------
e_ok=1
run_case E_cygpath_fails MSYS_NT-10.0 "$MSYS_PATH" 'exit 3'
[ "$RC" -ne 0 ] || {
	bad "E_cygpath_fails: expected nonzero exit, got 0"
	e_ok=0
}
[ -z "$OUT" ] || {
	bad "E_cygpath_fails: emitted a path despite a failed translation: $OUT"
	e_ok=0
}
printf '%s' "$ERR" | grep -q 'could not translate the fixture path' || {
	bad "E_cygpath_fails: stderr did not explain the failure: $ERR"
	e_ok=0
}
[ "$e_ok" -eq 1 ] && ok "E_cygpath_fails (exit=$RC, a failed translation is not a pass)"

# --- Scenario F — cygpath prints nothing: failure, no output. ----------------
f_ok=1
run_case F_cygpath_empty CYGWIN_NT-10.0 "$MSYS_PATH" 'exit 0'
[ "$RC" -ne 0 ] || {
	bad "F_cygpath_empty: expected nonzero exit, got 0"
	f_ok=0
}
[ -z "$OUT" ] || {
	bad "F_cygpath_empty: emitted a path despite an empty translation: $OUT"
	f_ok=0
}
printf '%s' "$ERR" | grep -q 'empty native path' || {
	bad "F_cygpath_empty: stderr did not explain the failure: $ERR"
	f_ok=0
}
[ "$f_ok" -eq 1 ] && ok "F_cygpath_empty (exit=$RC, an empty translation is not a pass)"

# --- Scenario G — no untranslated emission survives in the shipped script. ---
# Both exit routes (revalidated cache hit and fresh verified download) must print
# through the translator; a bare `printf '%s\n' "$EXE"` would reintroduce the bug
# on exactly one of them, which is how it went unnoticed for six CI rounds.
g_ok=1
raw_emissions="$(grep -c '^[[:space:]]*printf .*"\$EXE"[[:space:]]*$' "$SCRIPT" || true)"
[ "$raw_emissions" = "0" ] || {
	bad "G_no_raw_emit: $raw_emissions raw \$EXE emission(s) remain in $SCRIPT"
	g_ok=0
}
translated="$(grep -c '^[[:space:]]*emit_consumer_path "\$EXE"$' "$SCRIPT" || true)"
[ "$translated" = "2" ] || {
	bad "G_no_raw_emit: expected 2 translated emissions (cache hit + fresh download), found $translated"
	g_ok=0
}
[ "$g_ok" -eq 1 ] && ok "G_no_raw_emit (both exit routes emit through emit_consumer_path)"

printf '=== harness result: %d passed, %d failed ===\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ]
