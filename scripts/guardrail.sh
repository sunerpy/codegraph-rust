#!/bin/sh
# Scope guardrail: forbid AI/vector/LLM crates from being added to the workspace

FORBIDDEN='surrealdb|rig-|qdrant|lancedb|candle|onnx|\bort\b'
EXIT_CODE=0

# Find all Cargo.toml files, excluding reference/
for toml in $(find . -name 'Cargo.toml' -not -path '*/reference/*' -not -path '*/.cargo/*'); do
    # Grep for forbidden crates in [dependencies] sections
    if grep -E "^($FORBIDDEN) " "$toml" > /dev/null 2>&1; then
        line_num=$(grep -n -E "^($FORBIDDEN) " "$toml" | head -1 | cut -d: -f1)
        crate_name=$(grep -E "^($FORBIDDEN) " "$toml" | head -1 | awk '{print $1}')
        echo "❌ FORBIDDEN CRATE DETECTED: $crate_name at $toml:$line_num"
        EXIT_CODE=1
    fi
done

exit $EXIT_CODE
