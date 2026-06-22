#!/bin/sh
# codegraph one-liner installer (Linux / macOS).
#
#   curl -fsSL https://raw.githubusercontent.com/sunerpy/codegraph-rust/main/scripts/install.sh | sh
#
# Env overrides:
#   CODEGRAPH_VERSION      pin a release (e.g. 0.4.0 or v0.4.0); default: latest
#   CODEGRAPH_INSTALL_DIR  install destination; default: $HOME/.local/bin
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

asset="${BIN}-${version}-${target}.${ext}"
url="https://github.com/${REPO}/releases/download/v${version}/${asset}"

install_dir="${CODEGRAPH_INSTALL_DIR:-$HOME/.local/bin}"

info "Installing ${BIN} v${version} (${target})"
info "  from: ${url}"
info "  to:   ${install_dir}/${BIN}"

# Temp workspace, cleaned up on exit.
tmp=$(mktemp -d 2>/dev/null || mktemp -d -t codegraph)
trap 'rm -rf "$tmp"' EXIT INT TERM

download "$url" "$tmp/$asset" || err "download failed: $url"
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
