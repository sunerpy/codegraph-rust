#!/bin/sh
set -eu
script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd -P)
repo_root=$(CDPATH='' cd -- "$script_dir/.." && pwd -P)
cd "$repo_root"
forbidden='surrealdb|rig-|qdrant|lancedb|candle|onnx|\bort\b'
exit_code=0
if ! find . -name Cargo.toml -not -path '*/reference/*' -not -path '*/.cargo/*' \
    -exec sh -c '
        pattern=$1
        shift
        status=0
        for toml do
            if grep -E "^($pattern) " "$toml" >/dev/null 2>&1; then
                echo "FORBIDDEN CRATE: $(grep -E "^($pattern) " "$toml" | head -1) at $toml" >&2
                status=1
            fi
        done
        exit "$status"
    ' sh "$forbidden" {} +; then
    exit_code=1
fi
for gate in check-action-pins.sh check-asset-names.sh check-ci-gate.sh; do
    if ! bash "$script_dir/$gate" "$repo_root"; then
        echo "guardrail: $gate failed" >&2
        exit_code=1
    fi
done
exit "$exit_code"
