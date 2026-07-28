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

# CI/release gate integrity: `CI Success` is the one required status check AND the
# job the release gates on, but nothing else verifies that it is strict — a gate
# that accepts `cancelled`/`skipped`, or that stops requiring a job, still lets
# every test and lint pass. GitHub Actions also cannot express "require every job",
# so the required SET has to be asserted from outside the workflow. Same home and
# same reason as the block above: `guardrail` is the single step run by make ci,
# the pre-push hook, and the CI `test` job.
if ! bash "$SCRIPT_DIR/check-ci-gate.sh"; then
    echo "❌ CI GATE INTEGRITY: 'CI Success' does not strictly require every job to succeed."
    EXIT_CODE=1
fi

exit $EXIT_CODE
