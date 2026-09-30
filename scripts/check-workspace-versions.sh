#!/usr/bin/env bash
# check-workspace-versions.sh — deterministic workspace-version gate.
#
# Purpose
#   Enforce that all four version surfaces of this workspace agree exactly, and
#   that the workspace package set is identical between `cargo metadata` and the
#   source-less entries of Cargo.lock. This is the release-please invariant that
#   keeps the tag, binary, archive, and every in-repo version string in lockstep.
#
#   The four version surfaces:
#     1. root Cargo.toml [workspace.package] version
#     2. every source-less package version in Cargo.lock (workspace members)
#     3. the root (".") entry in .release-please-manifest.json
#     4. .github/scaffold.json placeholders.CURRENT_VERSION
#
# Cargo ordering contract (see the frozen v1.5 plan, E2)
#   * The FIRST operation snapshots the Cargo.lock bytes/hash.
#   * The FIRST — and only — Cargo subprocess this gate runs is:
#         cargo metadata --locked --no-deps --format-version 1
#     It obtains the authoritative workspace package set from that non-mutating
#     command and never resolves or mutates dependencies.
#   * No Cargo command may precede this gate in scripts, Make, hooks, or
#     workflows. Every later dependency-resolving Cargo command uses --locked.
#
# Determinism guarantee
#   On EVERY exit path (success or failure) an EXIT trap re-hashes Cargo.lock and
#   proves the bytes are byte-for-byte unchanged; a mutated lock is itself a
#   hard failure (exit 90).
#
# Usage
#   scripts/check-workspace-versions.sh [WORKSPACE_ROOT]
#   WORKSPACE_ROOT defaults to the repository root (the script's parent dir).

set -euo pipefail

# ---------------------------------------------------------------------------
# Resolve the workspace root (arg 1, else the repo root = script's ../).
# ---------------------------------------------------------------------------
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
WORKSPACE_ROOT="${1:-"$(cd -- "$SCRIPT_DIR/.." && pwd -P)"}"

CARGO_TOML="$WORKSPACE_ROOT/Cargo.toml"
CARGO_LOCK="$WORKSPACE_ROOT/Cargo.lock"
RELEASE_MANIFEST="$WORKSPACE_ROOT/.release-please-manifest.json"
RELEASE_CONFIG="$WORKSPACE_ROOT/release-please-config.json"
SCAFFOLD_CONFIG="$WORKSPACE_ROOT/.github/scaffold.json"

fail() {
    printf 'check-workspace-versions: ERROR: %s\n' "$1" >&2
    exit 1
}

for f in "$CARGO_TOML" "$CARGO_LOCK" "$RELEASE_MANIFEST" "$RELEASE_CONFIG" "$SCAFFOLD_CONFIG"; do
    [ -f "$f" ] || fail "required file not found: $f"
done

# ---------------------------------------------------------------------------
# FIRST OPERATION: snapshot the Cargo.lock bytes/hash, then arm the EXIT trap
# so the lock is proven unchanged on every subsequent exit path.
# ---------------------------------------------------------------------------
LOCK_SHA_PRE="$(sha256sum "$CARGO_LOCK" | awk '{print $1}')"

verify_lock_unchanged() {
    local rc=$?
    local post=""
    # Deterministic re-hash: a deleted or unreadable lock cannot match the
    # snapshot, so treat any hashing failure as `post=<MISSING>` (i.e. changed).
    # `|| true` keeps `set -e` from short-circuiting the trap before it can
    # report exit 90.
    if [ -f "$CARGO_LOCK" ] && [ -r "$CARGO_LOCK" ]; then
        post="$(sha256sum "$CARGO_LOCK" 2>/dev/null | awk '{print $1}')" || post=""
    fi
    [ -n "$post" ] || post="<MISSING-OR-UNREADABLE>"
    if [ "$post" != "$LOCK_SHA_PRE" ]; then
        printf 'check-workspace-versions: CRITICAL: Cargo.lock mutated during gate\n' >&2
        printf '  pre : %s\n' "$LOCK_SHA_PRE" >&2
        printf '  post: %s\n' "$post" >&2
        exit 90
    fi
    exit "$rc"
}
trap verify_lock_unchanged EXIT

# ---------------------------------------------------------------------------
# FIRST CARGO SUBPROCESS: cargo metadata --locked --no-deps --format-version 1
# Run from the workspace root (cwd) so the argv is EXACTLY that — no
# `--manifest-path` — while still working from any caller cwd. Non-mutating;
# yields the authoritative workspace package set. If the lock is stale relative
# to the manifests, --locked makes Cargo refuse (drift), which is itself a
# version-consistency failure this gate must surface.
# ---------------------------------------------------------------------------
META_JSON=""
META_ERR=""
if ! META_JSON="$(cd "$WORKSPACE_ROOT" && \
    cargo metadata --locked --no-deps --format-version 1 2>/tmp/cwv_meta_err.$$)"; then
    META_ERR="$(cat "/tmp/cwv_meta_err.$$" 2>/dev/null || true)"
    rm -f "/tmp/cwv_meta_err.$$"
    printf 'check-workspace-versions: FAIL: cargo metadata --locked refused — Cargo.lock is\n' >&2
    printf '  out of date relative to the manifests (version/package-set drift).\n' >&2
    printf '  This means the lock and the workspace manifests disagree; regenerate the\n' >&2
    printf '  lock so all workspace members match the workspace version.\n' >&2
    if [ -n "$META_ERR" ]; then
        printf '  cargo said:\n%s\n' "$META_ERR" | sed 's/^/    /' >&2
    fi
    exit 1
fi
rm -f "/tmp/cwv_meta_err.$$"

# Authoritative workspace package names + versions (workspace members only).
META_NAMES="$(printf '%s' "$META_JSON" | jq -r '.packages[].name' | tr -d '\r' | sort)"
META_VERSIONS="$(printf '%s' "$META_JSON" \
    | jq -r '.packages[] | "\(.name)=\(.version)"' | tr -d '\r' | sort)"

# ---------------------------------------------------------------------------
# Parse surface 1: root [workspace.package] version from Cargo.toml.
# ---------------------------------------------------------------------------
WORKSPACE_VERSION="$(awk '
    /^\[/ { in_wp = ($0 == "[workspace.package]") ? 1 : 0; next }
    in_wp && /^version[[:space:]]*=/ {
        line = $0
        sub(/^version[[:space:]]*=[[:space:]]*"/, "", line)
        sub(/".*/, "", line)
        print line
        exit
    }
' "$CARGO_TOML")"
[ -n "$WORKSPACE_VERSION" ] || fail "could not parse [workspace.package] version from Cargo.toml"

# ---------------------------------------------------------------------------
# Parse surface 2: source-less package name=version entries from Cargo.lock.
# A source-less entry is a [[package]] block with no `source =` line — i.e. a
# workspace member.
# ---------------------------------------------------------------------------
LOCK_ENTRIES="$(awk '
    /^\[\[package\]\]/ { name=""; ver=""; src=0; next }
    /^name = /   { match($0, /"[^"]*"/); name=substr($0, RSTART+1, RLENGTH-2); next }
    /^version = /{ match($0, /"[^"]*"/); ver=substr($0, RSTART+1, RLENGTH-2); next }
    /^source = / { src=1; next }
    /^$/ { if (name != "" && src == 0) print name "=" ver; name=""; ver=""; src=0 }
    END  { if (name != "" && src == 0) print name "=" ver }
' "$CARGO_LOCK" | sort)"
[ -n "$LOCK_ENTRIES" ] || fail "no source-less packages found in Cargo.lock"

LOCK_NAMES="$(printf '%s\n' "$LOCK_ENTRIES" | sed 's/=.*//' | sort)"

# ---------------------------------------------------------------------------
# Parse surfaces 3/4: release manifest and rendered-scaffold metadata.
# ---------------------------------------------------------------------------
RELEASE_MANIFEST_VALUE="$(jq -r '."."' "$RELEASE_MANIFEST" | tr -d '\r')"
if [ -z "$RELEASE_MANIFEST_VALUE" ] || [ "$RELEASE_MANIFEST_VALUE" = "null" ]; then
    fail "no root \".\" entry in .release-please-manifest.json"
fi
SCAFFOLD_VERSION="$(jq -r '.placeholders.CURRENT_VERSION // empty' "$SCAFFOLD_CONFIG" | tr -d '\r')"
[ -n "$SCAFFOLD_VERSION" ] || fail "no placeholders.CURRENT_VERSION in .github/scaffold.json"

# This repository has a virtual workspace root (`[workspace]` without
# `[package]`) and every member inherits `version.workspace = true`.
# release-please 17.6.0's Rust/CargoWorkspace updaters require literal
# `[package].version` strings, so the supported contract is the proven
# `simple` strategy plus explicit extra-file updates for every version surface.
jq -e '.packages["."]."release-type" == "simple"' "$RELEASE_CONFIG" >/dev/null \
    || fail "release-please root package must use the simple strategy for this virtual Cargo workspace"
jq -e '
  .packages["."]."extra-files"
  | any(.type == "toml"
      and .path == "Cargo.toml"
      and .jsonpath == "$.workspace.package.version")
' "$RELEASE_CONFIG" >/dev/null \
    || fail "release-please must update Cargo.toml workspace.package.version"
jq -e '
  .packages["."]."extra-files"
  | any(.type == "toml"
      and .path == "Cargo.lock"
      and .jsonpath == "$.package[?(!@.source)].version")
' "$RELEASE_CONFIG" >/dev/null \
    || fail "release-please must update every source-less Cargo.lock package version"
jq -e '
  .packages["."]."extra-files"
  | any(.type == "json"
      and .path == ".github/scaffold.json"
      and .jsonpath == "$.placeholders.CURRENT_VERSION")
' "$RELEASE_CONFIG" >/dev/null \
    || fail "release-please must update .github/scaffold.json CURRENT_VERSION"

# ---------------------------------------------------------------------------
# Assertions. Collect every discrepancy for a precise, deterministic report.
# ---------------------------------------------------------------------------
FAILURES=0
report() { printf 'check-workspace-versions: MISMATCH: %s\n' "$1" >&2; FAILURES=$((FAILURES + 1)); }

# Package-set equality: cargo metadata members == source-less Cargo.lock entries.
if [ "$META_NAMES" != "$LOCK_NAMES" ]; then
    report "workspace package set differs between cargo metadata and Cargo.lock (source-less)"
    printf '  only in cargo metadata:\n' >&2
    comm -23 <(printf '%s\n' "$META_NAMES") <(printf '%s\n' "$LOCK_NAMES") | sed 's/^/    /' >&2
    printf '  only in Cargo.lock (source-less):\n' >&2
    comm -13 <(printf '%s\n' "$META_NAMES") <(printf '%s\n' "$LOCK_NAMES") | sed 's/^/    /' >&2
fi

# Surface 1 is the reference version; assert surfaces 2/3/4 and metadata match.
if [ "$RELEASE_MANIFEST_VALUE" != "$WORKSPACE_VERSION" ]; then
    report ".release-please-manifest.json \".\" = '$RELEASE_MANIFEST_VALUE' != [workspace.package] version = '$WORKSPACE_VERSION'"
fi
if [ "$SCAFFOLD_VERSION" != "$WORKSPACE_VERSION" ]; then
    report ".github/scaffold.json CURRENT_VERSION = '$SCAFFOLD_VERSION' != [workspace.package] version = '$WORKSPACE_VERSION'"
fi

while IFS= read -r entry; do
    [ -n "$entry" ] || continue
    ename="${entry%%=*}"
    ever="${entry#*=}"
    if [ "$ever" != "$WORKSPACE_VERSION" ]; then
        report "Cargo.lock package '$ename' = '$ever' != [workspace.package] version = '$WORKSPACE_VERSION'"
    fi
done <<EOF
$LOCK_ENTRIES
EOF

while IFS= read -r entry; do
    [ -n "$entry" ] || continue
    ename="${entry%%=*}"
    ever="${entry#*=}"
    if [ "$ever" != "$WORKSPACE_VERSION" ]; then
        report "cargo metadata package '$ename' = '$ever' != [workspace.package] version = '$WORKSPACE_VERSION'"
    fi
done <<EOF
$META_VERSIONS
EOF

if [ "$FAILURES" -ne 0 ]; then
    printf 'check-workspace-versions: FAIL: %d version-surface mismatch(es)\n' "$FAILURES" >&2
    exit 1
fi

# ---------------------------------------------------------------------------
# Success report (stdout — useful evidence for the release/CI logs).
# ---------------------------------------------------------------------------
PKG_COUNT="$(printf '%s\n' "$LOCK_NAMES" | grep -c .)"
printf 'check-workspace-versions: OK\n'
printf '  first cargo subprocess : cargo metadata --locked --no-deps --format-version 1\n'
printf '  workspace version      : %s\n' "$WORKSPACE_VERSION"
printf '  release manifest "."   : %s\n' "$RELEASE_MANIFEST_VALUE"
printf '  scaffold version       : %s\n' "$SCAFFOLD_VERSION"
printf '  source-less packages   : %s (all at %s)\n' "$PKG_COUNT" "$WORKSPACE_VERSION"
printf '%s\n' "$LOCK_NAMES" | sed 's/^/    /'
# EXIT trap now proves Cargo.lock is byte-for-byte unchanged.
