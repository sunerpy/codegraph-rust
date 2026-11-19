#!/usr/bin/env bash
#
# Rebuild the screenshot corpora and capture the website's viewer screenshots into
# docs/site/public/screens/. docs/site/README.md describes the procedure.
#
# USAGE
#   CODEGRAPH_CHROME=/path/to/chrome docs/site/tools/capture-screens.sh [codegraph-binary] [out-dir]
#
# The corpus is this repository at CODEGRAPH_SCREENS_COMMIT, extracted into codegraph-rust/ inside a
# new temporary directory with the viewer bundle excluded, indexed by the given binary and served by
# its viewer on 127.0.0.1:CODEGRAPH_SCREENS_PORT. The temporary directory, the index in it included,
# is removed on exit. Needs git, tar, curl, mktemp and Node 22 or later.
#
# The Screens and Steps captures, and the Symbol and Flow captures with conditions, read a second
# corpus: the Angular RealWorld app (gothinkster/angular-realworld-example-app, MIT) at
# CODEGRAPH_SCREENS_APP_COMMIT, served on 127.0.0.1:CODEGRAPH_SCREENS_APP_PORT. Only src/,
# package.json, angular.json and tsconfig*.json are taken. CODEGRAPH_SCREENS_APP names a local
# checkout of that commit to copy them from; without it they are fetched from github.com, that
# commit alone, shallow and sparse.

set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../../.." && pwd)"
bin="${1:-codegraph}"
out="${2:-$repo/docs/site/public/screens}"
commit="${CODEGRAPH_SCREENS_COMMIT:-f7424cd3747e117aee526f774b0602291ce2b1e5}"
port="${CODEGRAPH_SCREENS_PORT:-4791}"
app_url=https://github.com/gothinkster/angular-realworld-example-app.git
app_commit="${CODEGRAPH_SCREENS_APP_COMMIT:-ea0dd032e0425142f684a870d6e233537baa6bb6}"
app_port="${CODEGRAPH_SCREENS_APP_PORT:-4792}"
# The same files, as sparse-checkout rules and as repository-relative pathspecs.
app_sparse=("/src/" "/package.json" "/angular.json" "/tsconfig*.json")
app_paths=("src" "package.json" "angular.json" "tsconfig*.json")

# A directory this run creates, so nothing from an earlier run reaches the corpus and nothing
# that existed before is deleted.
work="$(mktemp -d "${TMPDIR:-/tmp}/codegraph-screens.XXXXXX")"
corpus="$work/codegraph-rust"
app="$work/angular-realworld-example-app"
viewer=""
app_viewer=""
cleanup() {
  for pid in "$viewer" "$app_viewer"; do
    if [ -n "$pid" ]; then
      kill "$pid" 2>/dev/null || true
      wait "$pid" 2>/dev/null || true
    fi
  done
  rm -rf -- "$work"
}
trap cleanup EXIT

mkdir -p "$corpus/.codegraph"
git -C "$repo" archive "$commit" | tar -x -C "$corpus"
# The archive carries no local config; without this exclusion the minified viewer bundle adds
# thousands of one- and two-letter symbols (AGENTS.md).
printf '[app]\nname = "codegraph-rust"\n\n[indexing]\nexclude = ["crates/codegraph-ui/viewer/"]\n' \
  >"$corpus/.codegraph/config.toml"
CODEGRAPH_NO_DAEMON=1 "$bin" init "$corpus"

# Copy the tracked files of the app's pathspecs out of a checkout at app_commit.
copy_app() {
  local from="$1" files
  files="$(git -C "$from" ls-files -z -- "${app_paths[@]}" | tr '\0' '\n')"
  if [ -z "$files" ]; then
    echo "error: no files under ${app_paths[*]} in $from" >&2
    exit 1
  fi
  mkdir -p "$app"
  git -C "$from" ls-files -z -- "${app_paths[@]}" | tar -c -C "$from" --null -T - | tar -x -C "$app"
}
if [ -n "${CODEGRAPH_SCREENS_APP:-}" ]; then
  if [ "$(git -C "$CODEGRAPH_SCREENS_APP" rev-parse HEAD)" != "$app_commit" ]; then
    echo "error: $CODEGRAPH_SCREENS_APP is not at $app_commit" >&2
    exit 1
  fi
  if [ -n "$(git -C "$CODEGRAPH_SCREENS_APP" status --porcelain -- "${app_paths[@]}")" ]; then
    echo "error: $CODEGRAPH_SCREENS_APP has changes under ${app_paths[*]}" >&2
    exit 1
  fi
  copy_app "$CODEGRAPH_SCREENS_APP"
else
  git init -q "$work/app.git"
  git -C "$work/app.git" remote add origin "$app_url"
  printf '%s\n' "${app_sparse[@]}" >"$work/app.git/.git/info/sparse-checkout"
  git -C "$work/app.git" config core.sparseCheckout true
  git -C "$work/app.git" fetch -q --depth 1 --filter=blob:none --no-tags origin "$app_commit"
  git -C "$work/app.git" checkout -q --detach FETCH_HEAD
  if [ "$(git -C "$work/app.git" rev-parse HEAD)" != "$app_commit" ]; then
    echo "error: github.com gave $(git -C "$work/app.git" rev-parse HEAD), not $app_commit" >&2
    exit 1
  fi
  copy_app "$work/app.git"
fi
mkdir -p "$app/.codegraph"
printf '[app]\nname = "angular-realworld-example-app"\n' >"$app/.codegraph/config.toml"
CODEGRAPH_NO_DAEMON=1 "$bin" init "$app"

# Serve one corpus, its pid in the variable named $4 for cleanup, and wait until it answers.
serve() {
  local root="$1" at="$2" log="$3"
  CODEGRAPH_UI=1 "$bin" ui "$root" --no-open --port "$at" >"$log" 2>&1 &
  printf -v "$4" '%s' "$!"
  # shellcheck disable=SC2016 # $1 is expanded by the inner shell, which receives the port.
  if ! timeout 30 sh -c 'until curl -sf "http://127.0.0.1:$1/api/stats" >/dev/null; do sleep 1; done' _ "$at"; then
    echo "error: the viewer did not answer on port $at; its output:" >&2
    cat "$log" >&2
    exit 1
  fi
}
serve "$corpus" "$port" "$work/viewer.log" viewer
serve "$app" "$app_port" "$work/app-viewer.log" app_viewer

node "$here/capture-screens.mjs" "http://127.0.0.1:$port" "http://127.0.0.1:$app_port" "$out"
