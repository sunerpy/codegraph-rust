#!/usr/bin/env bash
# Package, unpack, and execute the locally built release bytes.
set -euo pipefail

root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

case "$(uname -s)" in
Linux)
  target="$(case "$(uname -m)" in x86_64|amd64) echo x86_64;; arm64|aarch64) echo aarch64;; *) exit 2;; esac)-unknown-linux-musl"
  binary="$root/dist/codegraph"
  archive="$work/codegraph-local-${target}.tar.gz"
  [ -x "$binary" ] || { echo "missing executable $binary" >&2; exit 1; }
  tar -czf "$archive" -C "$root/dist" codegraph
  mkdir "$work/unpacked"
  tar -xzf "$archive" -C "$work/unpacked"
  "$work/unpacked/codegraph" --version
  ;;
Darwin)
  target="$(case "$(uname -m)" in x86_64|amd64) echo x86_64;; arm64|aarch64) echo aarch64;; *) exit 2;; esac)-apple-darwin"
  binary="$root/dist/codegraph"
  archive="$work/codegraph-local-${target}.tar.gz"
  [ -x "$binary" ] || { echo "missing executable $binary" >&2; exit 1; }
  tar -czf "$archive" -C "$root/dist" codegraph
  mkdir "$work/unpacked"
  tar -xzf "$archive" -C "$work/unpacked"
  "$work/unpacked/codegraph" --version
  ;;
MINGW*|MSYS*|CYGWIN*)
  command -v pwsh >/dev/null 2>&1 || { echo "pwsh is required for the Windows archive smoke" >&2; exit 1; }
  binary="$root/dist/codegraph.exe"
  [ -f "$binary" ] || { echo "missing executable $binary" >&2; exit 1; }
  # shellcheck disable=SC2016 # PowerShell expands $env:* inside this literal.
  ROOT="$root" WORK="$work" pwsh -NoProfile -Command '
    $ErrorActionPreference = "Stop"
    Compress-Archive -LiteralPath "$env:ROOT/dist/codegraph.exe" -DestinationPath "$env:WORK/codegraph-local-x86_64-pc-windows-msvc.zip"
    Expand-Archive -LiteralPath "$env:WORK/codegraph-local-x86_64-pc-windows-msvc.zip" -DestinationPath "$env:WORK/unpacked"
    & "$env:WORK/unpacked/codegraph.exe" --version
  '
  ;;
*)
  echo "unsupported archive-smoke host: $(uname -s)" >&2
  exit 2
  ;;
esac
