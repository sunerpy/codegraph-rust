#!/usr/bin/env bash
# install-checksum.test.sh — fixture harness for the installer's SHA-256 gate.
#
# Drives scripts/install.sh against a LOCAL fake release directory. NO network
# access: a `curl` shim first on PATH resolves every requested URL to a file in
# the fixture release dir (and exits 22, curl's HTTP-error code, when the file is
# absent), so the script's own download/fetch plumbing is exercised unchanged.
#
# It proves the gate:
#   * matching checksum          -> install proceeds (exit 0, binary present),
#   * MISMATCHING checksum       -> abort (nonzero, binary ABSENT),
#   * mismatch + opt-out set     -> STILL aborts (opt-out never bypasses a mismatch),
#   * truncated archive          -> abort (real-world corrupt download),
#   * no sha256 tool             -> explicit refusal; installs only with opt-out,
#   * missing SHA256SUMS         -> explicit refusal; installs only with opt-out,
#   * SHA256SUMS without our line-> explicit refusal,
#   * CRLF SHA256SUMS            -> accepted (line endings must not break matching).
#
# Runs no Cargo and touches no repository file.
#
# Usage: scripts/tests/install-checksum.test.sh

set -euo pipefail

HARNESS_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
REPO_ROOT="$(cd -- "$HARNESS_DIR/../.." && pwd -P)"
INSTALLER="$REPO_ROOT/scripts/install.sh"
VERSION="9.9.9"

[ -f "$INSTALLER" ] || {
	printf 'harness: installer not found: %s\n' "$INSTALLER" >&2
	exit 2
}

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

# The asset name install.sh will compute on this host.
case "$(uname -s)" in
Linux) OS_PART="unknown-linux-musl" ;;
Darwin) OS_PART="apple-darwin" ;;
*)
	printf 'harness: unsupported host OS %s\n' "$(uname -s)" >&2
	exit 2
	;;
esac
case "$(uname -m)" in
x86_64 | amd64) ARCH_PART="x86_64" ;;
arm64 | aarch64) ARCH_PART="aarch64" ;;
*)
	printf 'harness: unsupported host arch %s\n' "$(uname -m)" >&2
	exit 2
	;;
esac
ASSET="codegraph-${VERSION}-${ARCH_PART}-${OS_PART}.tar.gz"

# ---------------------------------------------------------------------------
# Sandbox PATH. install.sh must find ONLY the tools we hand it, so the
# "no hashing tool" scenario is a genuine absence rather than a stubbed failure.
# ---------------------------------------------------------------------------
BASE_TOOLS=(sh tar gzip uname sed grep head mktemp cut awk tr rm mkdir mv chmod cat ls)
make_sandbox_bin() {
	local dir="$1" with_hash="$2" tool src
	mkdir -p "$dir"
	for tool in "${BASE_TOOLS[@]}"; do
		src="$(command -v "$tool")" || {
			printf 'harness: missing host tool %s\n' "$tool" >&2
			exit 2
		}
		ln -sf "$src" "$dir/$tool"
	done
	if [ "$with_hash" = "with_hash" ]; then
		src="$(command -v sha256sum)" || {
			printf 'harness: host has no sha256sum\n' >&2
			exit 2
		}
		ln -sf "$src" "$dir/sha256sum"
	fi
}

# ---------------------------------------------------------------------------
# curl shim: URL -> $RELEASE_DIR/<basename>. Absent file => exit 22.
# ---------------------------------------------------------------------------
write_curl_shim() {
	local dir="$1"
	mkdir -p "$dir"
	cat > "$dir/curl" <<'SHIM'
#!/bin/sh
# Offline stand-in for curl: resolves URLs to files in $CG_TEST_RELEASE_DIR.
# Deliberately depends on nothing beyond /bin/sh builtins so the sandboxed PATH
# (which omits a hashing tool in some scenarios) cannot accidentally help it.
set -u
url=""
out=""
while [ "$#" -gt 0 ]; do
	case "$1" in
	-o)
		out="$2"
		shift 2
		;;
	http://* | https://*)
		url="$1"
		shift
		;;
	*) shift ;;
	esac
done
[ -n "$url" ] || exit 2
src="$CG_TEST_RELEASE_DIR/${url##*/}"
[ -f "$src" ] || exit 22
if [ -n "$out" ]; then cat "$src" > "$out"; else cat "$src"; fi
SHIM
	chmod +x "$dir/curl"
}

# ---------------------------------------------------------------------------
# make_release DIR — a fake release: one tar.gz holding an executable stub
# `codegraph`, plus a decoy asset so SHA256SUMS is never a single-line file.
# ---------------------------------------------------------------------------
make_release() {
	local dir="$1" stage
	mkdir -p "$dir"
	stage="$dir/.stage"
	mkdir -p "$stage"
	cat > "$stage/codegraph" <<'STUB'
#!/bin/sh
printf 'codegraph 9.9.9 (test stub)\n'
STUB
	chmod +x "$stage/codegraph"
	tar -czf "$dir/$ASSET" -C "$stage" codegraph
	printf 'decoy\n' > "$dir/codegraph-${VERSION}-x86_64-pc-windows-msvc.zip"
	rm -rf "$stage"
}

sums_for() {
	local dir="$1" f
	(
		cd "$dir" || exit 1
		for f in $(printf '%s\n' *.tar.gz *.zip | LC_ALL=C sort); do
			sha256sum "$f"
		done
	)
}

# ---------------------------------------------------------------------------
# run_install NAME RELEASE_DIR WITH_HASH SKIP_ENV -> rc / out / err / installed
# ---------------------------------------------------------------------------
RC=0
OUT=""
ERR=""
INSTALLED_PATH=""
run_install() {
	local name="$1" release_dir="$2" with_hash="$3" skip_env="$4"
	local sandbox="$WORK/$name.bin" install_dir="$WORK/$name.dest"
	OUT="$WORK/$name.out"
	ERR="$WORK/$name.err"
	INSTALLED_PATH="$install_dir/codegraph"
	make_sandbox_bin "$sandbox" "$with_hash"
	write_curl_shim "$sandbox"
	mkdir -p "$install_dir"
	rm -f "$INSTALLED_PATH"
	set +e
	env -i \
		PATH="$sandbox" \
		HOME="$WORK/$name.home" \
		CG_TEST_RELEASE_DIR="$release_dir" \
		CODEGRAPH_VERSION="$VERSION" \
		CODEGRAPH_INSTALL_DIR="$install_dir" \
		CODEGRAPH_SKIP_CHECKSUM="$skip_env" \
		/bin/sh "$INSTALLER" > "$OUT" 2> "$ERR"
	RC=$?
	set -e
}

assert_installed() {
	local name="$1"
	[ -f "$INSTALLED_PATH" ] || {
		bad "$name: expected the binary at $INSTALLED_PATH, it is absent"
		return 1
	}
}
assert_not_installed() {
	local name="$1"
	if [ -e "$INSTALLED_PATH" ]; then
		bad "$name: binary WAS installed despite a failed verification"
		return 1
	fi
}
assert_rc() {
	local name="$1" want="$2"
	if [ "$RC" != "$want" ]; then
		bad "$name: expected exit $want, got $RC"
		note "stderr: $(tr '\n' '|' < "$ERR")"
		return 1
	fi
}
assert_rc_nonzero() {
	local name="$1"
	if [ "$RC" -eq 0 ]; then
		bad "$name: expected a NONZERO exit, got 0"
		note "stderr: $(tr '\n' '|' < "$ERR")"
		return 1
	fi
}
assert_stderr() {
	local name="$1" re="$2"
	if ! grep -Eq "$re" "$ERR"; then
		bad "$name: expected diagnostic /$re/ on stderr"
		note "stderr: $(tr '\n' '|' < "$ERR")"
		return 1
	fi
}

printf '=== installer checksum-gate fixture harness ===\n'
printf 'asset under test: %s\n' "$ASSET"

# --- Scenario A — matching checksum: install proceeds. --------------------
REL_A="$WORK/rel_a"
make_release "$REL_A"
sums_for "$REL_A" > "$REL_A/SHA256SUMS"
a_ok=1
run_install "A_match" "$REL_A" with_hash ""
assert_rc "A_match" 0 || a_ok=0
assert_installed "A_match" || a_ok=0
assert_stderr "A_match" 'sha256: OK' || a_ok=0
[ "$a_ok" -eq 1 ] && ok "A_match (exit=0, binary installed, checksum reported OK)"

# --- Scenario B — mismatching checksum: abort, nothing installed. ---------
REL_B="$WORK/rel_b"
make_release "$REL_B"
sums_for "$REL_B" | sed "s|^[0-9a-f]\{64\}\(  ${ASSET}\)$|$(printf '0%.0s' $(seq 64))\1|" > "$REL_B/SHA256SUMS"
b_ok=1
run_install "B_mismatch" "$REL_B" with_hash ""
assert_rc_nonzero "B_mismatch" || b_ok=0
assert_stderr "B_mismatch" 'checksum MISMATCH' || b_ok=0
assert_not_installed "B_mismatch" || b_ok=0
[ "$b_ok" -eq 1 ] && ok "B_mismatch (exit=$RC, MISMATCH reported, binary NOT installed)"

# --- Scenario C — mismatch + opt-out: STILL aborts. ----------------------
c_ok=1
run_install "C_mismatch_optout" "$REL_B" with_hash "1"
assert_rc_nonzero "C_mismatch_optout" || c_ok=0
assert_stderr "C_mismatch_optout" 'checksum MISMATCH' || c_ok=0
assert_not_installed "C_mismatch_optout" || c_ok=0
[ "$c_ok" -eq 1 ] && ok "C_mismatch_optout (exit=$RC, opt-out does NOT bypass a mismatch)"

# --- Scenario D — truncated archive against a good SHA256SUMS. -----------
REL_D="$WORK/rel_d"
make_release "$REL_D"
sums_for "$REL_D" > "$REL_D/SHA256SUMS"
head -c 64 "$REL_D/$ASSET" > "$REL_D/$ASSET.trunc"
mv "$REL_D/$ASSET.trunc" "$REL_D/$ASSET"
d_ok=1
run_install "D_truncated" "$REL_D" with_hash ""
assert_rc_nonzero "D_truncated" || d_ok=0
assert_stderr "D_truncated" 'checksum MISMATCH' || d_ok=0
assert_not_installed "D_truncated" || d_ok=0
[ "$d_ok" -eq 1 ] && ok "D_truncated (exit=$RC, corrupt download caught BEFORE extraction)"

# --- Scenario E — no sha256 tool: refuse, then allow with opt-out. --------
e_ok=1
run_install "E_notool_refuse" "$REL_A" without_hash ""
assert_rc_nonzero "E_notool_refuse" || e_ok=0
assert_stderr "E_notool_refuse" 'cannot verify the download: no sha256sum or shasum' || e_ok=0
assert_stderr "E_notool_refuse" 'CODEGRAPH_SKIP_CHECKSUM' || e_ok=0
assert_not_installed "E_notool_refuse" || e_ok=0
[ "$e_ok" -eq 1 ] && ok "E_notool_refuse (exit=$RC, explicit refusal, binary NOT installed)"

e2_ok=1
run_install "E_notool_optout" "$REL_A" without_hash "1"
assert_rc "E_notool_optout" 0 || e2_ok=0
assert_stderr "E_notool_optout" 'UNVERIFIED binary' || e2_ok=0
assert_installed "E_notool_optout" || e2_ok=0
[ "$e2_ok" -eq 1 ] && ok "E_notool_optout (exit=0, installs only under the explicit opt-out)"

# --- Scenario F — no SHA256SUMS at all (pre-checksum release). ------------
REL_F="$WORK/rel_f"
make_release "$REL_F"
f_ok=1
run_install "F_nosums_refuse" "$REL_F" with_hash ""
assert_rc_nonzero "F_nosums_refuse" || f_ok=0
assert_stderr "F_nosums_refuse" 'cannot verify the download: could not download' || f_ok=0
assert_not_installed "F_nosums_refuse" || f_ok=0
[ "$f_ok" -eq 1 ] && ok "F_nosums_refuse (exit=$RC, missing SHA256SUMS fails CLOSED)"

f2_ok=1
run_install "F_nosums_optout" "$REL_F" with_hash "1"
assert_rc "F_nosums_optout" 0 || f2_ok=0
assert_stderr "F_nosums_optout" 'UNVERIFIED binary' || f2_ok=0
assert_installed "F_nosums_optout" || f2_ok=0
[ "$f2_ok" -eq 1 ] && ok "F_nosums_optout (exit=0, legacy release installable only via opt-out)"

# --- Scenario G — SHA256SUMS present but with no line for OUR asset. -----
REL_G="$WORK/rel_g"
make_release "$REL_G"
sums_for "$REL_G" | grep -v -- "$ASSET" > "$REL_G/SHA256SUMS"
g_ok=1
[ -s "$REL_G/SHA256SUMS" ] || {
	bad "G_no_entry: fixture SHA256SUMS ended up empty"
	g_ok=0
}
run_install "G_no_entry" "$REL_G" with_hash ""
assert_rc_nonzero "G_no_entry" || g_ok=0
assert_stderr "G_no_entry" "SHA256SUMS has no entry for ${ASSET}" || g_ok=0
assert_not_installed "G_no_entry" || g_ok=0
[ "$g_ok" -eq 1 ] && ok "G_no_entry (exit=$RC, a sums file that omits our asset is NOT a pass)"

# --- Scenario H — CRLF SHA256SUMS with the correct digest. ----------------
REL_H="$WORK/rel_h"
make_release "$REL_H"
sums_for "$REL_H" | sed 's/$/\r/' > "$REL_H/SHA256SUMS"
h_ok=1
grep -q $'\r' "$REL_H/SHA256SUMS" || {
	bad "H_crlf: fixture is not actually CRLF"
	h_ok=0
}
run_install "H_crlf" "$REL_H" with_hash ""
assert_rc "H_crlf" 0 || h_ok=0
assert_stderr "H_crlf" 'sha256: OK' || h_ok=0
assert_installed "H_crlf" || h_ok=0
[ "$h_ok" -eq 1 ] && ok "H_crlf (exit=0, CRLF line endings do not defeat the match)"

printf '=== harness result: %d passed, %d failed ===\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ]
