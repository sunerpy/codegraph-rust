#!/usr/bin/env bash
# Fail closed unless every external GitHub Action uses one immutable 40-hex SHA.
set -euo pipefail
repo_root="${1:-$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)}"
python3 - "$repo_root" <<'PY'
from pathlib import Path
import re, sys
root=Path(sys.argv[1])
fail=[]
line_re=re.compile(r'^\s*-?\s*uses:\s*([^\s#]+)(?:\s+#\s*(.+))?\s*$')
sha_re=re.compile(r'^([A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+)@([0-9a-f]{40})$')
for path in sorted((root/'.github/workflows').glob('*.yml')):
    for no,line in enumerate(path.read_text(encoding='utf-8').splitlines(),1):
        m=line_re.match(line)
        if not m: continue
        value,note=m.groups()
        if value.startswith('./'): continue
        if not sha_re.match(value):
            fail.append(f'{path.relative_to(root)}:{no}: external action is not pinned to a full SHA: {value}')
        if not note:
            fail.append(f'{path.relative_to(root)}:{no}: pinned action lacks a human-readable tag/date annotation')
if fail:
    print('\n'.join(fail),file=sys.stderr)
    raise SystemExit(1)
print('check-action-pins: OK (all external actions use annotated 40-hex SHAs)')
PY
