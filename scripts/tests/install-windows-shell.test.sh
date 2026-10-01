#!/usr/bin/env bash
# install.sh under a Windows POSIX shell (Git Bash, MSYS2, Cygwin) points at
# the PowerShell installer and exits before downloading anything (upstream
# #1294); Linux and Darwin are unchanged.
set -euo pipefail
root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd -P)"
installer="$root/scripts/install.sh"
work=$(mktemp -d); trap 'rm -rf "$work"' EXIT
pass=0; fail=0

bin="$work/bin"; mkdir -p "$bin"
for tool in sh tar gzip sed head mktemp cut awk tr rm mkdir mv chmod cat sha256sum; do
  src=$(command -v "$tool") && ln -s "$src" "$bin/$tool"
done
cat >"$bin/uname" <<'SH'
#!/bin/sh
case "$1" in -s) echo "$CG_UNAME_S";; -m) echo x86_64;; *) echo "$CG_UNAME_S";; esac
SH
cat >"$bin/curl" <<'SH'
#!/bin/sh
echo "$*" >>"$CG_DOWNLOADS"
exit 22
SH
chmod +x "$bin/uname" "$bin/curl"

run() {
  rm -f "$work/downloads"
  set +e
  env -i PATH="$bin" HOME="$work/home" CG_UNAME_S="$1" CG_DOWNLOADS="$work/downloads" \
    /bin/sh "$installer" >"$work/out" 2>"$work/err"
  rc=$?; set -e
}
check() {
  if [ "$2" -eq 1 ]; then echo "PASS: $1"; pass=$((pass+1));
  else echo "FAIL: $1 (exit $rc)" >&2; cat "$work/err" >&2; fail=$((fail+1)); fi
}

for shell_os in MINGW64_NT-10.0-26100 MSYS_NT-10.0-26100 CYGWIN_NT-10.0-26100; do
  run "$shell_os"
  ok=1
  [ "$rc" -ne 0 ] || ok=0
  grep -Fq 'irm https://raw.githubusercontent.com/sunerpy/codegraph-rust/main/scripts/install.ps1 | iex' "$work/err" || ok=0
  [ ! -e "$work/downloads" ] || ok=0
  check "$shell_os" "$ok"
done

# Linux still gets past the OS check to the (failing, fake) download.
run Linux
ok=1
[ -e "$work/downloads" ] || ok=0
! grep -q 'install.ps1' "$work/err" || ok=0
check Linux "$ok"

echo "windows-shell harness: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
