#!/usr/bin/env bash
set -euo pipefail
root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd -P)"
gate="$root/scripts/check-ci-gate.sh"
work=$(mktemp -d); trap 'rm -rf "$work"' EXIT
pass=0; fail=0
fixture() {
  local name=$1
  local dir="$work/$name"
  mkdir -p "$dir/.github/workflows"
  cp "$root/.github/workflows/ci.yml" "$dir/.github/workflows/ci.yml"
  cp "$root/.github/workflows/release.yml" "$dir/.github/workflows/release.yml"
  cp "$root/codecov.yml" "$dir/codecov.yml"
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
  [ "$want" != zero ] || [ "$rc" -eq 0 ] || ok=0
  [ "$want" != nonzero ] || [ "$rc" -ne 0 ] || ok=0
  grep -Eq -- "$regex" "$work/$name.out" "$work/$name.err" || ok=0
  if [ "$ok" -eq 1 ]; then echo "PASS: $name"; pass=$((pass+1));
  else echo "FAIL: $name (exit $rc)" >&2; cat "$work/$name.out" "$work/$name.err" >&2; fail=$((fail+1)); fi
}

run A_repository "$root"; expect A_repository zero 'only all-success exits zero'
B=$(fixture B_pristine); run B_pristine "$B"; expect B_pristine zero 'release topology'
C=$(fixture C_needs); mutate "$C/.github/workflows/ci.yml" '    needs: [workspace-version, linux, windows-clippy, windows-test, macos-watcher, ui, audit]' '    needs: [workspace-version, linux, windows-clippy, macos-watcher, ui, audit]'; run C_needs "$C"; expect C_needs nonzero 'MISMATCH \[gate-needs\]'
D=$(fixture D_always); mutate "$D/.github/workflows/ci.yml" '    if: always()' '    if: success()'; run D_always "$D"; expect D_always nonzero 'MISMATCH \[gate-always\]'
E=$(fixture E_new_job); mutate "$E/.github/workflows/ci.yml" '  ci-success:' $'  fuzz:\n    name: Fuzz\n    runs-on: ubuntu-24.04\n    steps:\n      - run: echo fuzz\n\n  ci-success:'; run E_new_job "$E"; expect E_new_job nonzero 'fuzz'
F=$(fixture F_coverage); mutate "$F/codecov.yml" '        informational: true' '        informational: false'; run F_coverage "$F"; expect F_coverage nonzero 'MISMATCH \[coverage\]'
G=$(fixture G_source); mutate "$G/.github/workflows/release.yml" '        run: bash scripts/check-workspace-versions.sh' '        run: echo skipped'; run G_source "$G"; expect G_source nonzero 'first-Cargo version gate'
H=$(fixture H_verify); mutate "$H/.github/workflows/release.yml" '    needs: [release-please, source-gate]' '    needs: release-please'; run H_verify "$H"; expect H_verify nonzero 'verify must need release-please and source-gate'
I=$(fixture I_upload); mutate "$I/.github/workflows/release.yml" '    needs: [release-please, source-gate, verify-ci, verify, build-binaries]' '    needs: [release-please, source-gate, verify, build-binaries]'; run I_upload "$I"; expect I_upload nonzero 'upload-assets bypasses'
J=$(fixture J_exact_ci); mutate "$J/.github/workflows/release.yml" 'select(.name == "CI Success")] | last // {}' 'select(.name == "CI Maybe")] | last // {}'; run J_exact_ci "$J"; expect J_exact_ci nonzero 'verify-ci does not require exact-SHA CI Success'
K=$(fixture K_yaml); mutate "$K/.github/workflows/ci.yml" 'jobs:' 'jobs: [bad]\nignored:'; run K_yaml "$K"; expect K_yaml nonzero 'check-ci-gate: ERROR'
L=$(fixture L_pr_ci_on_push); mutate "$L/.github/workflows/release.yml" "if: github.event_name == 'workflow_dispatch' && inputs.ci_run_id != ''" "if: inputs.ci_run_id != ''"; run L_pr_ci_on_push "$L"; expect L_pr_ci_on_push nonzero 'reads inputs.ci_run_id outside workflow_dispatch'
M=$(fixture M_exact_skipped); mutate "$M/.github/workflows/release.yml" "if: steps.pr_ci.outputs.accepted != 'true'" "if: steps.pr_ci.outputs.accepted != 'false'"; run M_exact_skipped "$M"; expect M_exact_skipped nonzero 'exact-SHA wait step is skipped'
N=$(fixture N_pr_read); mutate "$N/.github/workflows/release.yml" $'      # commits/<sha>/pulls in the dispatch-only PR-CI step.\n      pull-requests: read\n' ''; run N_pr_read "$N"; expect N_pr_read nonzero 'lacks pull-requests: read'
O=$(fixture O_no_recheck); mutate "$O/.github/workflows/release.yml" "if: needs.verify-ci.outputs.pr_ci_accepted == 'true'" 'if: false'; run O_no_recheck "$O"; expect O_no_recheck nonzero 'does not re-check exact-SHA CI before publishing'
P=$(fixture P_publish_needs); mutate "$P/.github/workflows/release.yml" '    needs: [release-please, verify-ci, upload-assets]' '    needs: [release-please, upload-assets]'; run P_publish_needs "$P"; expect P_publish_needs nonzero 'publish-release must need verify-ci'

echo "ci-gate harness: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
