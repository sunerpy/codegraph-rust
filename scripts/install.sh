#!/bin/sh
# Install codegraph from a GitHub Release after mandatory SHA-256 verification.
#
# Environment:
#   CODEGRAPH_VERSION      release version, with or without v (default: latest)
#   CODEGRAPH_INSTALL_DIR  destination (default: $HOME/.local/bin)
set -eu

REPO="sunerpy/codegraph-rust"
BIN="codegraph"
SUMS="SHA256SUMS"

err() { printf 'error: %s\n' "$1" >&2; exit 1; }
info() { printf '%s\n' "$1" >&2; }

if command -v curl >/dev/null 2>&1; then
	download() { curl -fsSL "$1" -o "$2"; }
	fetch() { curl -fsSL "$1"; }
elif command -v wget >/dev/null 2>&1; then
	download() { wget -qO "$2" "$1"; }
	fetch() { wget -qO - "$1"; }
else
	err "curl or wget is required"
fi
command -v tar >/dev/null 2>&1 || err "tar is required"

if command -v sha256sum >/dev/null 2>&1; then
	sha256_of() { sha256sum "$1" | cut -d' ' -f1; }
elif command -v shasum >/dev/null 2>&1; then
	sha256_of() { shasum -a 256 "$1" | cut -d' ' -f1; }
else
	err "sha256sum or shasum is required; refusing an unverified install"
fi

case "$(uname -s)" in
Linux) os_part="unknown-linux-musl" ;;
Darwin) os_part="apple-darwin" ;;
*) err "unsupported OS: $(uname -s) (supported: Linux, Darwin)" ;;
esac
case "$(uname -m)" in
x86_64 | amd64) arch_part="x86_64" ;;
arm64 | aarch64) arch_part="aarch64" ;;
*) err "unsupported architecture: $(uname -m)" ;;
esac

target="${arch_part}-${os_part}"
ext="tar.gz"
if [ -n "${CODEGRAPH_VERSION:-}" ]; then
	version=$(printf '%s' "$CODEGRAPH_VERSION" | sed 's/^v//')
else
	info "Resolving latest release..."
	api="https://api.github.com/repos/${REPO}/releases/latest"
	tag=$(fetch "$api" | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -1)
	[ -n "$tag" ] || err "could not resolve latest release"
	version=$(printf '%s' "$tag" | sed 's/^v//')
fi

asset="${BIN}-${version}-${target}.${ext}"
base_url="https://github.com/${REPO}/releases/download/v${version}"
install_dir="${CODEGRAPH_INSTALL_DIR:-$HOME/.local/bin}"
tmp=$(mktemp -d 2>/dev/null || mktemp -d -t codegraph)
trap 'rm -rf "$tmp"' EXIT INT TERM

info "Downloading ${asset}"
download "${base_url}/${asset}" "$tmp/$asset" || err "download failed: ${asset}"
download "${base_url}/${SUMS}" "$tmp/$SUMS" || err "could not download ${SUMS}; refusing an unverified install"

expected=$(tr -d '\r' <"$tmp/$SUMS" | awk -v want="$asset" '
	{ name=$2; sub(/^\*/, "", name); if (name == want) { print $1; exit } }')
[ -n "$expected" ] || err "${SUMS} has no entry for ${asset}; refusing an unverified install"
actual=$(sha256_of "$tmp/$asset")
[ "$actual" = "$expected" ] || err "checksum mismatch for ${asset}; refusing a corrupted or tampered archive"
info "sha256: OK (${actual})"

tar -xzf "$tmp/$asset" -C "$tmp" || err "failed to extract ${asset}"
[ -f "$tmp/$BIN" ] || err "archive did not contain ${BIN}"
mkdir -p "$install_dir"
mv "$tmp/$BIN" "$install_dir/$BIN"
chmod +x "$install_dir/$BIN"
info "Installed ${BIN} to ${install_dir}/${BIN}"
"$install_dir/$BIN" --version
case ":$PATH:" in
*":$install_dir:"*) ;;
*) info "Add ${install_dir} to PATH" ;;
esac
