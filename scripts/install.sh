#!/bin/sh
# codegraph one-liner installer (Linux / macOS).
#
#   curl -fsSL https://raw.githubusercontent.com/sunerpy/codegraph-rust/main/scripts/install.sh | sh
#
# Env overrides:
#   CODEGRAPH_VERSION      pin a release (e.g. 0.4.0 or v0.4.0); default: latest
#   CODEGRAPH_INSTALL_DIR  install destination; default: $HOME/.local/bin
#   CODEGRAPH_SKIP_CHECKSUM
#                          set to any non-empty value to proceed when the
#                          download CANNOT be verified — i.e. no sha256 tool is
#                          available, or the release has no usable SHA256SUMS
#                          (releases cut before checksums were published).
#                          This is an explicit opt-out: without it the installer
#                          REFUSES to install rather than run an unverified
#                          binary. It never bypasses a checksum MISMATCH — a
#                          mismatch always aborts.
set -eu

REPO="sunerpy/codegraph-rust"
BIN="codegraph"

err() {
	printf 'error: %s\n' "$1" >&2
	exit 1
}

info() {
	printf '%s\n' "$1" >&2
}

# Pick a downloader once, expose `download <url> <dest>`.
if command -v curl >/dev/null 2>&1; then
	download() { curl -fsSL "$1" -o "$2"; }
	fetch() { curl -fsSL "$1"; }
elif command -v wget >/dev/null 2>&1; then
	download() { wget -qO "$2" "$1"; }
	fetch() { wget -qO - "$1"; }
else
	err "need curl or wget to download releases"
fi

command -v tar >/dev/null 2>&1 || err "need tar to extract the release archive"

# Pick a SHA-256 tool once, expose `sha256_of <file>` printing the bare lowercase
# hex digest. `sha256sum` is coreutils (Linux); `shasum -a 256` is the macOS/perl
# fallback. If neither exists, sha256_tool stays empty and verification is
# impossible — handled explicitly at verify time, never silently skipped.
if command -v sha256sum >/dev/null 2>&1; then
	sha256_tool="sha256sum"
	sha256_of() { sha256sum "$1" | cut -d' ' -f1; }
elif command -v shasum >/dev/null 2>&1; then
	sha256_tool="shasum"
	sha256_of() { shasum -a 256 "$1" | cut -d' ' -f1; }
else
	sha256_tool=""
	sha256_of() { err "internal: sha256_of called with no hashing tool"; }
fi

# cannot_verify REASON — fail closed unless the operator explicitly opted out.
cannot_verify() {
	if [ "${CODEGRAPH_SKIP_CHECKSUM:-}" != "" ]; then
		info "WARNING: cannot verify download ($1)."
		info "WARNING: CODEGRAPH_SKIP_CHECKSUM is set — installing an UNVERIFIED binary."
		return 0
	fi
	printf 'error: cannot verify the download: %s\n' "$1" >&2
	printf 'error: refusing to install an unverified binary.\n' >&2
	printf 'error: to proceed anyway, re-run with CODEGRAPH_SKIP_CHECKSUM=1\n' >&2
	exit 1
}

# Detect OS.
os=$(uname -s)
case "$os" in
Linux) os_part="unknown-linux-musl" ;;
Darwin) os_part="apple-darwin" ;;
*) err "unsupported OS: $os (supported: Linux, Darwin)" ;;
esac

# Detect arch.
arch=$(uname -m)
case "$arch" in
x86_64 | amd64) arch_part="x86_64" ;;
arm64 | aarch64) arch_part="aarch64" ;;
*) err "unsupported arch: $arch (supported: x86_64, aarch64)" ;;
esac

target="${arch_part}-${os_part}"
ext="tar.gz"

# Resolve version: env override or latest-release API.
if [ "${CODEGRAPH_VERSION:-}" != "" ]; then
	version=$(printf '%s' "$CODEGRAPH_VERSION" | sed 's/^v//')
else
	info "Resolving latest release..."
	api="https://api.github.com/repos/${REPO}/releases/latest"
	tag=$(fetch "$api" | grep -o '"tag_name"[ ]*:[ ]*"[^"]*"' | head -1 | sed 's/.*"tag_name"[ ]*:[ ]*"\([^"]*\)".*/\1/')
	[ "${tag:-}" != "" ] || err "could not resolve latest release tag from $api"
	version=$(printf '%s' "$tag" | sed 's/^v//')
fi

SUMS="SHA256SUMS"
asset="${BIN}-${version}-${target}.${ext}"
release_base="https://github.com/${REPO}/releases/download/v${version}"
url="${release_base}/${asset}"
sums_url="${release_base}/${SUMS}"

install_dir="${CODEGRAPH_INSTALL_DIR:-$HOME/.local/bin}"

info "Installing ${BIN} v${version} (${target})"
info "  from: ${url}"
info "  to:   ${install_dir}/${BIN}"

# Temp workspace, cleaned up on exit.
tmp=$(mktemp -d 2>/dev/null || mktemp -d -t codegraph)
trap 'rm -rf "$tmp"' EXIT INT TERM

download "$url" "$tmp/$asset" || err "download failed: $url"

# Integrity gate — runs BEFORE extraction, so an unverified archive is never
# unpacked and its binary is never executed.
if [ "$sha256_tool" = "" ]; then
	cannot_verify "no sha256sum or shasum on PATH"
elif ! download "$sums_url" "$tmp/$SUMS" 2>/dev/null; then
	cannot_verify "could not download $sums_url"
else
	# Match the asset's own line. Tolerate CRLF (a SHA256SUMS that travelled
	# through a Windows editor) and both the `sha256sum` two-space and the
	# `shasum`/BSD single-space-star separators.
	expected=$(tr -d '\r' < "$tmp/$SUMS" | awk -v want="$asset" '
		{ name = $2; sub(/^\*/, "", name); if (name == want) { print $1; exit } }')
	if [ "${expected:-}" = "" ]; then
		cannot_verify "$SUMS has no entry for $asset"
	else
		actual=$(sha256_of "$tmp/$asset")
		if [ "$actual" != "$expected" ]; then
			printf 'error: checksum MISMATCH for %s\n' "$asset" >&2
			printf 'error:   expected %s\n' "$expected" >&2
			printf 'error:   actual   %s\n' "$actual" >&2
			printf 'error: refusing to install a corrupted or tampered archive.\n' >&2
			exit 1
		fi
		info "  sha256: OK (${actual})"
	fi
fi

tar -xzf "$tmp/$asset" -C "$tmp" || err "failed to extract $asset"

[ -f "$tmp/$BIN" ] || err "archive did not contain expected binary '$BIN'"

mkdir -p "$install_dir"
mv "$tmp/$BIN" "$install_dir/$BIN"
chmod +x "$install_dir/$BIN"

info "Installed: $install_dir/$BIN"
"$install_dir/$BIN" --version

# PATH hint if the install dir isn't already reachable.
case ":${PATH}:" in
*":${install_dir}:"*) ;;
*)
	info ""
	info "Note: ${install_dir} is not on your PATH. Add it, e.g.:"
	info "  export PATH=\"${install_dir}:\$PATH\""
	;;
esac

info ""
info "Done. Run '${BIN} --help' to get started."
