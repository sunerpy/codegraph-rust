#!/usr/bin/env bash
# shellcheck disable=SC2016
set -euo pipefail
root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd -P)"
gate="$root/scripts/check-asset-names.sh"
work=$(mktemp -d); trap 'rm -rf "$work"' EXIT
pass=0; fail=0
fixture() {
  local name=$1
  local dir="$work/$name"
  mkdir -p "$dir/.github/workflows" "$dir/scripts"
  cp "$root/.github/workflows/release.yml" "$dir/.github/workflows/release.yml"
  cp "$root/scripts/install.sh" "$dir/scripts/install.sh"
  cp "$root/scripts/install.ps1" "$dir/scripts/install.ps1"
  printf '%s' "$dir"
}
mutate() {
  CG_FILE=$1 CG_OLD=$2 CG_NEW=$3 python3 - <<'PY'
import os
p=os.environ['CG_FILE']; old=os.environ['CG_OLD']; new=os.environ['CG_NEW']
s=open(p,encoding='utf-8').read()
if old not in s: raise SystemExit(f'mutation anchor missing: {old!r}')
open(p,'w',encoding='utf-8').write(s.replace(old,new,1))
PY
}
run() { set +e; bash "$gate" "$2" >"$work/$1.out" 2>"$work/$1.err"; rc=$?; set -e; }
expect() {
  local name=$1 want=$2 regex=$3 ok=1
  if [ "$want" = zero ] && [ "$rc" -ne 0 ]; then ok=0; fi
  if [ "$want" = nonzero ] && [ "$rc" -eq 0 ]; then ok=0; fi
  if ! grep -Eq -- "$regex" "$work/$name.out" "$work/$name.err"; then ok=0; fi
  if [ "$ok" -eq 1 ]; then echo "PASS: $name"; pass=$((pass+1));
  else echo "FAIL: $name (exit $rc, expected $want /$regex/)" >&2; cat "$work/$name.out" "$work/$name.err" >&2; fail=$((fail+1)); fi
}

run A_repository "$root"; expect A_repository zero 'check-asset-names: OK'
B=$(fixture B_pristine); run B_pristine "$B"; expect B_pristine zero 'matrix targets   : 6'
C=$(fixture C_cosmetic); mutate "$C/.github/workflows/release.yml" '          pattern: dist-*' '          pattern: "dist-*"'; run C_cosmetic "$C"; expect C_cosmetic zero 'check-asset-names: OK'
D=$(fixture D_archive_name); mutate "$D/.github/workflows/release.yml" '${BINARY_NAME}-${VERSION}-${TARGET}.${ARCHIVE_KIND}' '${BINARY_NAME}_${VERSION}-${TARGET}.${ARCHIVE_KIND}'; run D_archive_name "$D"; expect D_archive_name nonzero 'MISMATCH \[archive-name\]'
E=$(fixture E_binary); mutate "$E/scripts/install.sh" 'BIN="codegraph"' 'BIN="codegraf"'; run E_binary "$E"; expect E_binary nonzero 'MISMATCH \[binary-name\]'
F=$(fixture F_download); mutate "$F/.github/workflows/release.yml" '          pattern: dist-*' '          pattern: bins-*'; run F_download "$F"; expect F_download nonzero 'MISMATCH \[artifact-plumbing\]'
G=$(fixture G_arm); mutate "$G/scripts/install.ps1" "    '^(ARM64|aarch64)$'    { \$archPart = 'aarch64' }" ''; run G_arm "$G"; expect G_arm nonzero 'MISMATCH \[target-coverage\]'
H=$(fixture H_remote_exact); mutate "$H/.github/workflows/release.yml" '--asset dist/SHA256SUMS --exact' '--asset dist/SHA256SUMS'; run H_remote_exact "$H"; expect H_remote_exact nonzero 'MISMATCH \[remote-assets\]'
I=$(fixture I_bypass); mutate "$I/scripts/install.sh" '# Install codegraph' '# CODEGRAPH_SKIP_CHECKSUM would be unsafe\n# Install codegraph'; run I_bypass "$I"; expect I_bypass nonzero 'checksum bypass detected'
J=$(fixture J_yaml); mutate "$J/.github/workflows/release.yml" 'jobs:' 'jobs: [not-a-mapping]\nignored:'; run J_yaml "$J"; expect J_yaml nonzero 'check-asset-names: ERROR'

echo "asset-name harness: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
