#!/usr/bin/env bash
set -euo pipefail
root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd -P)"
installer="$root/scripts/install.sh"; version=9.9.9
work=$(mktemp -d); trap 'rm -rf "$work"' EXIT
case "$(uname -s)" in Linux) os=unknown-linux-musl;; Darwin) os=apple-darwin;; *) exit 2;; esac
case "$(uname -m)" in x86_64|amd64) arch=x86_64;; arm64|aarch64) arch=aarch64;; *) exit 2;; esac
asset="codegraph-${version}-${arch}-${os}.tar.gz"
pass=0; fail=0
make_release() {
  local dir=$1; mkdir -p "$dir/stage"
  printf '#!/bin/sh\necho "codegraph 9.9.9"\n' >"$dir/stage/codegraph"; chmod +x "$dir/stage/codegraph"
  tar -czf "$dir/$asset" -C "$dir/stage" codegraph
  printf decoy >"$dir/decoy.zip"
}
sums() { (cd "$1" && sha256sum "$asset" decoy.zip); }
make_bin() {
  local dir=$1 hash=$2 tool src; mkdir -p "$dir"
  for tool in sh tar gzip uname sed head mktemp cut awk tr rm mkdir mv chmod cat; do src=$(command -v "$tool"); ln -s "$src" "$dir/$tool"; done
  [ "$hash" = yes ] && ln -s "$(command -v sha256sum)" "$dir/sha256sum"
  cat >"$dir/curl" <<'SH'
#!/bin/sh
url= out=
while [ "$#" -gt 0 ]; do case "$1" in -o) out=$2; shift 2;; http*) url=$1; shift;; *) shift;; esac; done
src="$CG_RELEASE/${url##*/}"; [ -f "$src" ] || exit 22
[ -n "$out" ] && cat "$src" >"$out" || cat "$src"
SH
  chmod +x "$dir/curl"
}
run() {
  local name=$1 rel=$2 hash=$3 legacy=${4:-}
  local bin="$work/$name.bin" dest="$work/$name.dest"
  make_bin "$bin" "$hash"; mkdir -p "$dest"
  set +e
  env -i PATH="$bin" HOME="$work/home" CG_RELEASE="$rel" CODEGRAPH_VERSION="$version" CODEGRAPH_INSTALL_DIR="$dest" CODEGRAPH_SKIP_CHECKSUM="$legacy" /bin/sh "$installer" >"$work/$name.out" 2>"$work/$name.err"
  rc=$?; set -e; installed="$dest/codegraph"
}
expect() {
  local name=$1 want=$2 present=$3 regex=$4 ok=1
  [ "$want" != zero ] || [ "$rc" -eq 0 ] || ok=0
  [ "$want" != nonzero ] || [ "$rc" -ne 0 ] || ok=0
  [ "$present" != yes ] || [ -f "$installed" ] || ok=0
  [ "$present" != no ] || [ ! -e "$installed" ] || ok=0
  grep -Eqi -- "$regex" "$work/$name.err" || ok=0
  if [ "$ok" -eq 1 ]; then echo "PASS: $name"; pass=$((pass+1));
  else echo "FAIL: $name (exit $rc)" >&2; cat "$work/$name.err" >&2; fail=$((fail+1)); fi
}
A=$work/A; make_release "$A"; sums "$A" >"$A/SHA256SUMS"; run A_match "$A" yes; expect A_match zero yes 'sha256: OK'
B=$work/B; make_release "$B"; sums "$B" | sed '1s/^[0-9a-f]\{64\}/0000000000000000000000000000000000000000000000000000000000000000/' >"$B/SHA256SUMS"; run B_mismatch "$B" yes 1; expect B_mismatch nonzero no 'checksum mismatch'
C=$work/C; make_release "$C"; sums "$C" >"$C/SHA256SUMS"; head -c 32 "$C/$asset" >"$C/truncated"; mv "$C/truncated" "$C/$asset"; run C_truncated "$C" yes; expect C_truncated nonzero no 'checksum mismatch'
run D_no_hash "$A" no 1; expect D_no_hash nonzero no 'refusing an unverified install'
E=$work/E; make_release "$E"; run E_no_sums "$E" yes 1; expect E_no_sums nonzero no 'refusing an unverified install'
F=$work/F; make_release "$F"; sha256sum "$F/decoy.zip" | sed "s|$F/||" >"$F/SHA256SUMS"; run F_no_entry "$F" yes 1; expect F_no_entry nonzero no 'no entry.*refusing an unverified install'
G=$work/G; make_release "$G"; sums "$G" | sed 's/$/\r/' >"$G/SHA256SUMS"; run G_crlf "$G" yes; expect G_crlf zero yes 'sha256: OK'

echo "installer harness: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
