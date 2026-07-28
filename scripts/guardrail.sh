#!/bin/sh
# Scope guardrail: forbid AI/vector/LLM crates from being added to the workspace,
# and hold the release asset-name contract (see the second block below).

SCRIPT_DIR=$(dirname "$0")
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

# Release asset-name contract: the archive name is assembled independently in the
# release workflow and in both installers, and nothing else in CI links those
# strings — drift there is invisible until AFTER a release is published. This gate
# lives here rather than in a Cargo test because it parses CI/shell/PowerShell
# files, not Rust, and `guardrail` is the one step run by make ci, the pre-push
# hook, AND both CI jobs.
if ! bash "$SCRIPT_DIR/check-asset-names.sh"; then
    echo "❌ RELEASE ASSET-NAME DRIFT: the release workflow and the installers disagree."
    EXIT_CODE=1
fi

exit $EXIT_CODE
