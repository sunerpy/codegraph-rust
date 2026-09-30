#!/usr/bin/env bash
# Verify the six-target archive/install/checksum contract from the real files.
set -euo pipefail
repo_root="${1:-$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)}"
CG_ROOT="$repo_root" python3 <<'PY'
from pathlib import Path
import fnmatch, os, re, sys
try:
    import yaml
except ImportError:
    print('check-asset-names: ERROR: PyYAML is required',file=sys.stderr); raise SystemExit(2)
root=Path(os.environ['CG_ROOT'])
workflow=root/'.github/workflows/release.yml'
sh_path=root/'scripts/install.sh'; ps_path=root/'scripts/install.ps1'

def die(msg):
    print(f'check-asset-names: ERROR: {msg}',file=sys.stderr); raise SystemExit(2)
def fail(area,msg): failures.append((area,msg))
def load(path):
    try: return path.read_text(encoding='utf-8')
    except OSError as e: die(f'cannot read {path}: {e}')
failures=[]
wf_text=load(workflow); sh=load(sh_path); ps=load(ps_path)
try: wf=yaml.safe_load(wf_text)
except Exception as e: die(f'release.yml is not valid YAML: {e}')
try: jobs=wf['jobs']; build=jobs['build-binaries']; upload=jobs['upload-assets']; publish=jobs['publish-release']
except Exception as e: die(f'release workflow shape is incomplete: {e}')
wf_bin=(wf.get('env') or {}).get('BINARY_NAME')
if not wf_bin: die('env.BINARY_NAME is missing')
matrix=((build.get('strategy') or {}).get('matrix') or {}).get('include')
if not isinstance(matrix,list) or not matrix: die('build-binaries matrix is missing')
targets=[]
for entry in matrix:
    if not isinstance(entry,dict) or not entry.get('target') or not entry.get('archive'):
        die(f'invalid matrix entry: {entry!r}')
    targets.append((str(entry['target']),str(entry['archive'])))
if len(targets)!=6 or len({t for t,_ in targets})!=6: fail('matrix',f'expected six unique targets, got {targets}')
steps=build.get('steps') or []
runs='\n'.join(str(s.get('run','')) for s in steps if isinstance(s,dict))
canonical='${BINARY_NAME}-${VERSION}-${TARGET}.${ARCHIVE_KIND}'
if canonical not in runs: fail('archive-name',f'archive computation must contain {canonical!r}')
for required in ('tar -czf "dist/${ARCHIVE}"','Compress-Archive','dist/${env:ARCHIVE}','unpacked/$BINARY_NAME','unpacked/${env:BINARY_NAME}.exe'):
    if required not in runs: fail('package-smoke',f'missing package/unpack/smoke token: {required}')
up_action=next((s for s in steps if isinstance(s,dict) and str(s.get('uses','')).startswith('actions/upload-artifact@')),None)
if not up_action: die('build-binaries does not upload artifacts')
up_with=up_action.get('with') or {}
if str(up_with.get('name'))!='dist-${{ matrix.target }}' or str(up_with.get('path'))!='dist/*':
    fail('artifact-plumbing',f'unexpected upload contract: {up_with}')
u_steps=upload.get('steps') or []
down=next((s for s in u_steps if isinstance(s,dict) and str(s.get('uses','')).startswith('actions/download-artifact@')),None)
if not down: die('upload-assets does not download build artifacts')
dw=down.get('with') or {}
if dw.get('pattern')!='dist-*' or dw.get('merge-multiple') is not True:
    fail('artifact-plumbing',f'download contract must use dist-* with merge-multiple: true, got {dw}')
u_runs='\n'.join(str(s.get('run','')) for s in u_steps if isinstance(s,dict))
p_runs='\n'.join(str(s.get('run','')) for s in (publish.get('steps') or []) if isinstance(s,dict))
for token in ('*.tar.gz *.zip','sha256sum >SHA256SUMS','gh release upload','dist/SHA256SUMS','--verify-only','--checksums dist/SHA256SUMS','--asset dist/SHA256SUMS','--exact'):
    if token not in u_runs: fail('remote-assets',f'upload-assets missing {token!r}')
for token in ('--publish','--checksums dist/SHA256SUMS','--asset dist/SHA256SUMS','--exact'):
    if token not in p_runs: fail('remote-assets',f'publish-release missing {token!r}')

def sh_assign(name):
    m=re.search(rf'^{re.escape(name)}="([^"]*)"',sh,re.M); return m.group(1) if m else None
def ps_assign(name):
    m=re.search(rf'^\${re.escape(name)}\s*=\s*[\'\"]([^\'\"]*)[\'\"]',ps,re.M|re.I); return m.group(1) if m else None
sh_bin=sh_assign('BIN'); ps_bin=ps_assign('Bin')
sh_sums=sh_assign('SUMS'); ps_sums=ps_assign('sums')
if not (wf_bin==sh_bin==ps_bin): fail('binary-name',f'workflow={wf_bin!r}, install.sh={sh_bin!r}, install.ps1={ps_bin!r}')
if not (sh_sums==ps_sums=='SHA256SUMS'): fail('checksums',f'installers disagree: {sh_sums!r}, {ps_sums!r}')
if 'SKIP_CHECKSUM' in sh or 'SKIP_CHECKSUM' in ps: fail('checksums','checksum bypass detected; installers must fail closed')
sh_os=re.findall(r'os_part="([^"]+)"',sh); sh_arch=re.findall(r'arch_part="([^"]+)"',sh)
sh_targets={f'{a}-{o}' for a in sh_arch for o in sh_os}
ps_arch=set(re.findall(r"\$archPart\s*=\s*'([^']+)'",ps))
ps_targets={f'{a}-pc-windows-msvc' for a in ps_arch}
for target,archive in targets:
    owners=(target in sh_targets)+(target in ps_targets)
    if owners!=1: fail('target-coverage',f'{target} must be produced by exactly one installer')
    expected='tar.gz' if target in sh_targets else 'zip'
    if archive!=expected: fail('target-coverage',f'{target} uses {archive}, installer expects {expected}')
for target in sorted(sh_targets|ps_targets):
    if target not in {t for t,_ in targets}: fail('target-coverage',f'installer can request unbuilt target {target}')
if failures:
    for area,msg in failures: print(f'check-asset-names: MISMATCH [{area}]: {msg}',file=sys.stderr)
    print(f'check-asset-names: FAIL: {len(failures)} problem(s)',file=sys.stderr); raise SystemExit(1)
print('check-asset-names: OK')
print(f'  binary/checksums : {wf_bin}, SHA256SUMS (mandatory)')
print(f'  archive skeleton : <bin>-<version>-<target>.<ext>')
print(f'  matrix targets   : {len(targets)}')
for target,archive in targets: print(f'    {target:28} {archive}')
print('  release verify   : remote name + size + sha256, exact asset set')
PY
