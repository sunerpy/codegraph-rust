#!/usr/bin/env bash
# setup-legacy-fixture.sh — materialize the frozen v0.40.4 legacy CLI binary.
#
# Batch M legacy-compatibility tests execute the PUBLISHED v0.40.4 release
# binary (tag v0.40.4 / commit aba40799ecacb94515f7e1690914d2accc4c8973) to
# prove that an unmodified OLD scanner cannot open or mutate v2 storage. This
# script is the only sanctioned way to obtain that binary.
#
# Contract (deliberately strict — a legacy test must never silently "pass"
# against a binary that is not the real v0.40.4 release):
#
#   1. The asset for the CURRENT NATIVE host is selected from the checked-in
#      manifest. An unpinned host is a hard error; no cross-execution, no
#      emulation, no substitution of a locally built binary.
#   2. The archive is downloaded over HTTPS from the immutable release-download
#      URL and its SHA-256 must equal the manifest digest. Redirects are
#      followed, but only the FINAL bytes are trusted (the digest is computed
#      over what landed on disk).
#   3. Exactly ONE archive member — the manifest's `member` name — is extracted.
#      Archive-supplied paths are never honored, so a crafted archive cannot
#      write outside the destination.
#   4. Every declared integrity field is checked: the downloaded archive's SIZE
#      (`archive_size`) and SHA-256 (`archive_sha256`), then the extracted
#      executable's SHA-256 and its exact `--version` stdout. The SHA-256 values
#      remain the authority; the size is an additional declared-field check, not
#      a substitute for it.
#   5. The cache is DIGEST-ADDRESSED (`<exe-sha256>/<member>`) and revalidated
#      on EVERY run. A stale or corrupt cached file is re-downloaded, never
#      trusted.
#   6. Missing network, a size or digest mismatch, a missing member, or a wrong
#      `--version` all exit NONZERO with an actionable message. There is no
#      skip path: an unavailable fixture is a fixture-setup FAILURE.
#   7. Every failure path is swept: an EXIT trap removes the staging directory
#      and any partial archive. Cleanup NEVER touches an executable that has
#      already passed full verification.
#
# Usage:
#   scripts/setup-legacy-fixture.sh              # prepare (download if needed),
#                                                # then print the exe path
#   scripts/setup-legacy-fixture.sh --print      # print the path ONLY if the
#                                                # cached exe already validates;
#                                                # never downloads
# `--print` is the only accepted argument; anything else is a usage error.
#
# Output: the absolute path of the verified legacy executable on stdout (last
# line), expressed in the path domain of the CONSUMER, not of this shell. All
# diagnostics go to stderr. Tests consume the path via CODEGRAPH_LEGACY_BIN,
# which this script's caller exports.
#
# WHY the output domain is called out: on Windows this script runs under an
# MSYS/Cygwin bash (Git Bash on the CI runner), whose absolute paths — `/tmp/x`,
# `/c/Users/...` — are a VIRTUAL namespace understood only by the MSYS runtime.
# The consumer is a NATIVE Win32 process (`cargo test`), which resolves the
# string literally through the Win32 API and cannot see that namespace: a
# perfectly valid `/tmp/...` fixture path fails there with `os error 3`, "The
# system cannot find the path specified". So the final path is translated to a
# native Win32 form before it is printed; every INTERNAL use keeps the shell's
# own form. On Linux and macOS there is one path domain and nothing is
# translated.

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd -P)"
MANIFEST="$REPO_ROOT/crates/codegraph-cli/tests/fixtures/legacy-v0.40.4/manifest.toml"

# Cache root: overridable so CI can point at a cached directory. Default keeps
# the artifacts out of the source tree entirely.
CACHE_ROOT="${CODEGRAPH_LEGACY_FIXTURE_CACHE:-${TMPDIR:-/tmp}/codegraph-legacy-fixture}"

# Swept on EVERY exit path (success, `die`, `set -e` abort, signal). Populated
# only with the in-flight staging directory and partial archive; a fully
# verified executable is never registered here.
STAGE_DIR=""
PARTIAL_ARCHIVE=""

sweep() {
    [ -n "$STAGE_DIR" ] && [ -d "$STAGE_DIR" ] && rm -rf "$STAGE_DIR"
    [ -n "$PARTIAL_ARCHIVE" ] && [ -e "$PARTIAL_ARCHIVE" ] && rm -f "$PARTIAL_ARCHIVE"
    return 0
}
trap sweep EXIT HUP INT TERM

# print_usage FD — the usage text, kept in one place so docs cannot drift.
print_usage() {
    cat >&"$1" <<'USAGE'
usage: setup-legacy-fixture.sh [--print]
  (no argument)  prepare the frozen v0.40.4 fixture (download if needed) and
                 print the verified executable path
  --print        print the path ONLY if the cached executable already validates;
                 never downloads
USAGE
}

die() {
    printf 'setup-legacy-fixture: %s\n' "$1" >&2
    exit 1
}

need() {
    command -v "$1" >/dev/null 2>&1 || die "required tool not found on PATH: $1"
}

# ---------------------------------------------------------------------------
# Manifest reading. The manifest is a small, fixed-shape TOML file authored in
# this repo, so a line-oriented reader is sufficient and avoids adding a TOML
# dependency to a shell script. Every lookup FAILS LOUDLY when absent.
# ---------------------------------------------------------------------------

[ -f "$MANIFEST" ] || die "fixture manifest missing: $MANIFEST"

# fixture_field KEY -> value of `KEY = "value"` inside the [fixture] table.
fixture_field() {
    local key="$1" value
    value="$(
        awk -v key="$key" '
            /^\[/ { in_fixture = ($0 == "[fixture]"); next }
            in_fixture && $1 == key {
                # strip up to the first quote, then the trailing quote
                line = $0
                sub(/^[^"]*"/, "", line)
                sub(/"[^"]*$/, "", line)
                print line
                exit
            }
        ' "$MANIFEST"
    )"
    [ -n "$value" ] || die "manifest [fixture] is missing key '$key'"
    printf '%s' "$value"
}

# asset_field TARGET KEY -> value of KEY inside the [[asset]] block whose
# `target` equals TARGET. Numeric values are returned unquoted.
asset_field() {
    local target="$1" key="$2" value
    value="$(
        awk -v want="$target" -v key="$key" '
            /^\[\[asset\]\]/ { current = ""; matched = 0; next }
            /^\[/ { current = ""; matched = 0; next }
            $1 == "target" {
                line = $0
                sub(/^[^"]*"/, "", line); sub(/"[^"]*$/, "", line)
                current = line
                matched = (current == want)
                next
            }
            matched && $1 == key {
                line = $0
                sub(/^[[:space:]]*[A-Za-z0-9_]+[[:space:]]*=[[:space:]]*/, "", line)
                gsub(/^"|"$/, "", line)
                print line
                exit
            }
        ' "$MANIFEST"
    )"
    [ -n "$value" ] || die "manifest [[asset]] target='$target' is missing key '$key'"
    printf '%s' "$value"
}

# ---------------------------------------------------------------------------
# Native host selection. NO cross-execution: the host's own OS/arch decides.
# ---------------------------------------------------------------------------

detect_target() {
    local os arch uname_s uname_m
    uname_s="$(uname -s 2>/dev/null || printf 'unknown')"
    uname_m="$(uname -m 2>/dev/null || printf 'unknown')"

    case "$uname_s" in
        Linux) os="linux" ;;
        MINGW* | MSYS* | CYGWIN* | Windows_NT) os="windows" ;;
        *) os="$uname_s" ;;
    esac
    case "$uname_m" in
        x86_64 | amd64) arch="x86_64" ;;
        *) arch="$uname_m" ;;
    esac

    case "$os/$arch" in
        linux/x86_64) printf 'x86_64-unknown-linux-musl' ;;
        windows/x86_64) printf 'x86_64-pc-windows-msvc' ;;
        *)
            die "no pinned v0.40.4 asset for this host ($os/$arch).
The legacy-compatibility fixture executes the REAL published binary natively.
Pinned hosts: linux/x86_64 and windows/x86_64 (see $MANIFEST).
This host cannot run the legacy fixture; do NOT interpret that as coverage."
            ;;
    esac
}

sha256_of() {
    local path="$1"
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$path" | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$path" | awk '{print $1}'
    else
        die "no sha256 tool available (need sha256sum or shasum)"
    fi
}

download() {
    local url="$1" dest="$2"
    if command -v curl >/dev/null 2>&1; then
        # -f: HTTP errors are failures; -L: follow redirects (final bytes are
        # digest-verified below, so a redirect cannot substitute content).
        curl -fsSL --retry 3 --retry-delay 2 --connect-timeout 20 -o "$dest" "$url" \
            || return 1
    elif command -v wget >/dev/null 2>&1; then
        wget -q --tries=3 --timeout=20 -O "$dest" "$url" || return 1
    else
        die "no downloader available (need curl or wget)"
    fi
}

# extract_member ARCHIVE FORMAT MEMBER DEST
#   Extracts exactly MEMBER to the file DEST. The archive's own path is never
#   used as a filesystem destination, so `../` members cannot escape.
extract_member() {
    local archive="$1" format="$2" member="$3" dest="$4" stage
    stage="$(mktemp -d "$CACHE_ROOT/extract.XXXXXX")"
    STAGE_DIR="$stage"
    case "$format" in
        tar.gz)
            need tar
            # Refuse an archive whose member set is not exactly the expected name.
            local listing
            listing="$(tar -tzf "$archive")" || die "cannot list archive: $archive"
            [ "$listing" = "$member" ] \
                || die "archive member set is not exactly '$member': $listing"
            tar -xzf "$archive" -C "$stage" "$member" \
                || die "cannot extract '$member' from $archive"
            ;;
        zip)
            need unzip
            local listing
            listing="$(unzip -Z1 "$archive")" || die "cannot list archive: $archive"
            [ "$listing" = "$member" ] \
                || die "archive member set is not exactly '$member': $listing"
            # -j flattens any path component; the member name is matched exactly.
            unzip -o -j -q "$archive" "$member" -d "$stage" \
                || die "cannot extract '$member' from $archive"
            ;;
        *) die "unsupported archive format: $format" ;;
    esac
    [ -f "$stage/$(basename "$member")" ] \
        || die "expected member '$member' absent after extraction"
    mv -f "$stage/$(basename "$member")" "$dest"
    rm -rf "$stage"
    STAGE_DIR=""
}

# emit_consumer_path PATH — print PATH in the path domain of the NATIVE consumer.
#
# Windows only, and only when `cygpath` is actually present: MSYS/Cygwin paths
# are invisible to a Win32 process (see the header). `cygpath -m` yields a native
# absolute path with FORWARD slashes (`C:/Users/...`), which Win32 accepts and
# which needs no backslash escaping when it travels through $GITHUB_ENV and Rust
# string handling. A missing or failing `cygpath` is fatal, not silently skipped:
# emitting an MSYS path there would hand the consumer a path it cannot open, and
# a fixture that cannot be verified is a setup FAILURE. Every other host has a
# single path domain and prints the path verbatim.
emit_consumer_path() {
    local path="$1" native
    case "$(uname -s 2>/dev/null || printf 'unknown')" in
        MINGW* | MSYS* | CYGWIN* | Windows_NT) ;;
        *)
            printf '%s\n' "$path"
            return 0
            ;;
    esac
    command -v cygpath > /dev/null 2>&1 \
        || die "cygpath not found on PATH.
This shell reports a Windows host, so the fixture path must be translated from
the MSYS/Cygwin namespace ('$path') into a native Win32 path before a NATIVE
consumer such as 'cargo test' can open it. Without cygpath that translation
cannot be performed, and emitting the untranslated path would fail later with
'The system cannot find the path specified'."
    native="$(cygpath -m -- "$path")" \
        || die "cygpath could not translate the fixture path to a native Win32 path: $path"
    [ -n "$native" ] || die "cygpath returned an empty native path for: $path"
    printf '%s\n' "$native"
}

size_of() {
    local path="$1" size
    if size="$(wc -c <"$path" 2>/dev/null)"; then
        printf '%s' "${size// /}"
    else
        die "cannot measure size of $path"
    fi
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

PRINT_ONLY=0
case "${1-}" in
    "") ;;
    --print) PRINT_ONLY=1 ;;
    -h | --help)
        print_usage 1
        exit 0
        ;;
    *)
        printf 'setup-legacy-fixture: unknown argument: %s\n' "$1" >&2
        print_usage 2
        exit 2
        ;;
esac
if [ "$#" -gt 1 ]; then
    printf 'setup-legacy-fixture: too many arguments\n' >&2
    print_usage 2
    exit 2
fi

TAG="$(fixture_field tag)"
COMMIT="$(fixture_field commit)"
EXPECTED_VERSION="$(fixture_field expected_version_stdout)"

TARGET="$(detect_target)"
URL="$(asset_field "$TARGET" url)"
ARCHIVE_NAME="$(asset_field "$TARGET" archive_name)"
ARCHIVE_FORMAT="$(asset_field "$TARGET" archive_format)"
ARCHIVE_SHA="$(asset_field "$TARGET" archive_sha256)"
ARCHIVE_SIZE="$(asset_field "$TARGET" archive_size)"
MEMBER="$(asset_field "$TARGET" member)"
EXE_SHA="$(asset_field "$TARGET" executable_sha256)"

case "$ARCHIVE_SIZE" in
    '' | *[!0-9]*) die "manifest archive_size for $TARGET is not a decimal byte count: '$ARCHIVE_SIZE'" ;;
esac

mkdir -p "$CACHE_ROOT"
# Digest-addressed cache slot: a different digest can never collide with this one.
SLOT="$CACHE_ROOT/$EXE_SHA"
EXE="$SLOT/$MEMBER"
mkdir -p "$SLOT"

revalidate() {
    # Revalidate the cached executable on EVERY run: digest first, then the
    # real `--version`. Any failure means the cache is not trustworthy.
    [ -f "$EXE" ] || return 1
    [ "$(sha256_of "$EXE")" = "$EXE_SHA" ] || return 1
    chmod +x "$EXE" 2>/dev/null || true
    local observed
    observed="$("$EXE" --version 2>/dev/null | tr -d '\r')" || return 1
    [ "$observed" = "$EXPECTED_VERSION" ] || return 1
    return 0
}

if revalidate; then
    emit_consumer_path "$EXE"
    exit 0
fi

if [ "$PRINT_ONLY" -eq 1 ]; then
    die "--print requested but the cached executable does not validate.
  slot: $SLOT
Run without --print to download and verify the frozen $TAG asset."
fi

printf 'setup-legacy-fixture: preparing %s %s (%s)\n' "$TAG" "$TARGET" "$COMMIT" >&2
rm -f "$EXE"
ARCHIVE="$SLOT/$ARCHIVE_NAME"
rm -f "$ARCHIVE"
PARTIAL_ARCHIVE="$ARCHIVE"
if ! download "$URL" "$ARCHIVE"; then
    die "cannot download the frozen legacy fixture asset.
  url: $URL
This is a FIXTURE-SETUP FAILURE, not a skipped test. The legacy-compatibility
tests require the real published $TAG binary. Restore network access (or
pre-populate CODEGRAPH_LEGACY_FIXTURE_CACHE=$CACHE_ROOT with the verified
$EXE_SHA slot) and rerun."
fi

observed_size="$(size_of "$ARCHIVE")"
if [ "$observed_size" != "$ARCHIVE_SIZE" ]; then
    die "archive size mismatch for $ARCHIVE_NAME
  expected: $ARCHIVE_SIZE bytes
  observed: $observed_size bytes
The downloaded bytes are not the frozen $TAG asset; refusing to run legacy tests."
fi

observed_archive="$(sha256_of "$ARCHIVE")"
if [ "$observed_archive" != "$ARCHIVE_SHA" ]; then
    die "archive SHA-256 mismatch for $ARCHIVE_NAME
  expected: $ARCHIVE_SHA
  observed: $observed_archive
The downloaded bytes are not the frozen $TAG asset; refusing to run legacy tests."
fi

extract_member "$ARCHIVE" "$ARCHIVE_FORMAT" "$MEMBER" "$EXE"
rm -f "$ARCHIVE"
PARTIAL_ARCHIVE=""
chmod +x "$EXE" 2>/dev/null || true

observed_exe="$(sha256_of "$EXE")"
if [ "$observed_exe" != "$EXE_SHA" ]; then
    rm -f "$EXE"
    die "extracted executable SHA-256 mismatch for $MEMBER
  expected: $EXE_SHA
  observed: $observed_exe"
fi

observed_version="$("$EXE" --version 2>/dev/null | tr -d '\r')" || {
    rm -f "$EXE"
    die "extracted executable could not run '--version' natively on this host"
}
if [ "$observed_version" != "$EXPECTED_VERSION" ]; then
    rm -f "$EXE"
    die "legacy executable reports '$observed_version', expected '$EXPECTED_VERSION'"
fi
printf 'setup-legacy-fixture: verified %s\n' "$EXE_SHA" >&2

emit_consumer_path "$EXE"
