#!/usr/bin/env bash
# Prove that CI Success is strict and that the same-run release gate is wired.
set -euo pipefail
repo_root="${1:-$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)}"
work=$(mktemp -d); trap 'rm -rf "$work"' EXIT
body="$work/body.sh"; jobs_out="$work/jobs.txt"
CG_ROOT="$repo_root" CG_BODY="$body" CG_JOBS="$jobs_out" python3 <<'PY'
from pathlib import Path
import os,re,sys
try: import yaml
except ImportError: print('check-ci-gate: ERROR: PyYAML is required',file=sys.stderr); raise SystemExit(2)
root=Path(os.environ['CG_ROOT']); failures=[]
def die(msg): print(f'check-ci-gate: ERROR: {msg}',file=sys.stderr); raise SystemExit(2)
def fail(area,msg): failures.append((area,msg))
def load(path):
    try: data=yaml.safe_load(path.read_text(encoding='utf-8'))
    except Exception as e: die(f'cannot parse {path.name}: {e}')
    if not isinstance(data,dict): die(f'{path.name} is not a mapping')
    return data
ci=load(root/'.github/workflows/ci.yml'); rel=load(root/'.github/workflows/release.yml'); cov=load(root/'codecov.yml')
ci_jobs=ci.get('jobs')
if not isinstance(ci_jobs,dict): die('ci.yml has no jobs')
gate=ci_jobs.get('ci-success')
if not isinstance(gate,dict): die('ci-success is missing')
if gate.get('name')!='CI Success': fail('identity',f"name is {gate.get('name')!r}")
if str(gate.get('if','')).strip() not in ('always()','${{ always() }}'): fail('gate-always','ci-success must use if: always()')
needs=gate.get('needs'); needs=[needs] if isinstance(needs,str) else needs
if not isinstance(needs,list) or not needs: die('ci-success needs is empty')
actual=sorted(map(str,needs)); expected=sorted(set(ci_jobs)-{'ci-success','coverage'})
if actual!=expected: fail('gate-needs',f'needs={actual}, expected every blocking job={expected}')
status=((cov.get('coverage') or {}).get('status') or {})
for name,cfg in status.items():
    default=(cfg or {}).get('default') if isinstance(cfg,dict) else None
    if not isinstance(default,dict) or default.get('informational') is not True:
        fail('coverage',f'codecov {name} is not informational')
steps=gate.get('steps') or []; run_steps=[s for s in steps if isinstance(s,dict) and isinstance(s.get('run'),str)]
if len(run_steps)!=1: die(f'ci-success must have one run step, got {len(run_steps)}')
step=run_steps[0]; env=step.get('env') or {}
if not re.match(r'^\$\{\{\s*toJSON\(\s*needs\s*\)\s*\}\}$',str(env.get('NEEDS_JSON','')).strip()):
    fail('gate-shape','ci-success must read env.NEEDS_JSON from toJSON(needs)')
if re.search(r'needs\.[A-Za-z0-9_-]+\.result',step['run']): fail('gate-shape','gate body hard-codes a job result')
Path(os.environ['CG_BODY']).write_text(step['run']+'\n',encoding='utf-8')
Path(os.environ['CG_JOBS']).write_text('\n'.join(actual)+'\n',encoding='utf-8')
rel_jobs=rel.get('jobs')
if not isinstance(rel_jobs,dict): die('release.yml has no jobs')
for job in ('release-please','source-gate','verify-ci','verify','build-binaries','upload-assets','publish-release'):
    if job not in rel_jobs: fail('release-shape',f'{job} is missing')
def needset(job):
    value=(rel_jobs.get(job) or {}).get('needs',[]); return {value} if isinstance(value,str) else set(map(str,value or []))
def runs(job):
    return '\n'.join(str(s.get('run','')) for s in ((rel_jobs.get(job) or {}).get('steps') or []) if isinstance(s,dict))
if 'release-please' not in needset('source-gate'): fail('release-gate','source-gate must need release-please')
if 'scripts/check-workspace-versions.sh' not in runs('source-gate'): fail('release-gate','source-gate does not run the first-Cargo version gate')
if 'release-please' not in needset('verify-ci'): fail('release-gate','verify-ci must need release-please')
verify_ci_body=runs('verify-ci')
if 'select(.name == "CI Success")' not in verify_ci_body or 'conclusion' not in verify_ci_body or '= success' not in verify_ci_body:
    fail('release-gate','verify-ci does not require exact-SHA CI Success')
if not {'release-please','source-gate'} <= needset('verify'): fail('release-gate','verify must need release-please and source-gate')
if 'make check' not in runs('verify'): fail('release-gate','verify does not run the complete local gate')
if not {'release-please','source-gate'} <= needset('build-binaries'): fail('release-gate','build-binaries bypasses source-gate')
if not {'release-please','source-gate','verify-ci','verify','build-binaries'} <= needset('upload-assets'): fail('release-gate','upload-assets bypasses a required predecessor')
if not {'release-please','upload-assets'} <= needset('publish-release'): fail('release-gate','publish-release must need upload-assets')
if '--verify-only' not in runs('upload-assets') or '--exact' not in runs('upload-assets'): fail('release-gate','upload-assets lacks exact remote verification')
if '--publish' not in runs('publish-release') or '--exact' not in runs('publish-release'): fail('release-gate','terminal publish lacks exact remote re-verification')
if failures:
    for area,msg in failures: print(f'check-ci-gate: MISMATCH [{area}]: {msg}',file=sys.stderr)
    print(f'check-ci-gate: FAIL: {len(failures)} problem(s)',file=sys.stderr); raise SystemExit(1)
print('check-ci-gate: structure OK')
print('  required jobs      : '+', '.join(actual))
print('  excluded            : coverage (informational)')
print('  release topology    : source-gate + exact-SHA CI -> verify/build -> upload -> exact publish')
PY
mapfile -t required <"$jobs_out"
needs_json() {
  local base=$1 override=${2:-} value=${3:-} out='{}' job result
  for job in "${required[@]}"; do
    result=$base; [ -n "$override" ] && [ "$job" = "$override" ] && result=$value
    out=$(jq -cn --argjson acc "$out" --arg k "$job" --arg v "$result" '$acc + {($k): {result: $v}}')
  done
  printf '%s' "$out"
}
run_case() {
  local label=$1 want=$2 payload=$3 rc
  set +e; NEEDS_JSON="$payload" bash "$body" >"$work/out" 2>&1; rc=$?; set -e
  if { [ "$want" = zero ] && [ "$rc" -ne 0 ]; } || { [ "$want" = nonzero ] && [ "$rc" -eq 0 ]; }; then
    echo "check-ci-gate: truth-table failure: $label (exit $rc)" >&2; cat "$work/out" >&2; return 1
  fi
  printf '  %-42s exit %s OK\n' "$label" "$rc"
}
run_case 'all success' zero "$(needs_json success)"
first=${required[0]}
for value in failure cancelled skipped neutral timed_out unknown ''; do
  run_case "$first = ${value:-<empty>}" nonzero "$(needs_json success "$first" "$value")"
done
for job in "${required[@]}"; do run_case "$job = cancelled" nonzero "$(needs_json success "$job" cancelled)"; done
run_case 'empty context' nonzero ''
run_case 'empty object' nonzero '{}'
run_case 'malformed context' nonzero 'not-json'
run_case 'missing result key' nonzero '{"x":{}}'
echo 'check-ci-gate: OK (only all-success exits zero)'
