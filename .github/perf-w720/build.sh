#!/usr/bin/env bash
# W7-20 CI perf: the fast release builds, each from its own detached worktree
# of this checkout, with the fast release profile of the local runs
# (per-commit-precision-builds): 6a58caa8 (base, W7-19i, bin-base), the W7-20
# candidate commit (cand, bin-cand) and, when a pre commit is given, fec4fbb
# (pre, bin-pre; OD-B20's cumulative line on typescript-large).
# usage: build.sh <private-dir> <target-dir> <base> <cand> [<pre>]
set -euo pipefail
shopt -s inherit_errexit
dir=$1
export CARGO_TARGET_DIR=$2
export CARGO_PROFILE_RELEASE_LTO=off CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 \
       CARGO_PROFILE_RELEASE_OPT_LEVEL=2 CARGO_PROFILE_RELEASE_INCREMENTAL=true
build() { # bin-dir commit
  local tree="$dir/trees/$1"
  git worktree add --detach "$tree" "$2" > /dev/null
  (cd "$tree" && cargo build --locked --release -p codegraph-rs -p codegraph-bench)
  mkdir -p "$dir/$1"
  cp "$CARGO_TARGET_DIR/release/codegraph" "$CARGO_TARGET_DIR/release/bench" "$dir/$1/"
  echo "$1 = commit $(git rev-parse "$2") tree $(git rev-parse "$2^{tree}")"
  sha256sum "$dir/$1/codegraph" "$dir/$1/bench"
  git worktree remove --force "$tree"
}
build bin-base "$3"
build bin-cand "$4"
if [ -n "${5:-}" ]; then build bin-pre "$5"; fi
