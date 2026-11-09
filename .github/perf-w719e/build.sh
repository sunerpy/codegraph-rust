#!/usr/bin/env bash
# W7-19e CI perf: the two fast release builds, each from its own detached
# worktree of this checkout, with the fast release profile of the local runs
# (per-commit-precision-builds): 97d5b2e (base, W7-19d, bin-base) and the
# W7-19e candidate commit (cand, bin-cand).
# usage: build.sh <private-dir> <target-dir> <base> <cand>
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
}
build bin-base "$3"
build bin-cand "$4"
