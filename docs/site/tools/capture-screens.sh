#!/usr/bin/env bash
#
# Rebuild the screenshot corpus and capture the website's viewer screenshots into
# docs/site/public/screens/. docs/site/README.md describes the procedure.
#
# USAGE
#   CODEGRAPH_CHROME=/path/to/chrome docs/site/tools/capture-screens.sh [codegraph-binary] [out-dir]
#
# The corpus is this repository at CODEGRAPH_SCREENS_COMMIT, extracted into codegraph-rust/ inside a
# new temporary directory with the viewer bundle excluded, indexed by the given binary and served by
# its viewer on 127.0.0.1:CODEGRAPH_SCREENS_PORT. The temporary directory, the index in it included,
# is removed on exit. Needs git, tar, curl, mktemp and Node 22 or later.

set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../../.." && pwd)"
bin="${1:-codegraph}"
out="${2:-$repo/docs/site/public/screens}"
commit="${CODEGRAPH_SCREENS_COMMIT:-f7424cd3747e117aee526f774b0602291ce2b1e5}"
port="${CODEGRAPH_SCREENS_PORT:-4791}"

# A directory this run creates, so nothing from an earlier run reaches the corpus and nothing
# that existed before is deleted.
work="$(mktemp -d "${TMPDIR:-/tmp}/codegraph-screens.XXXXXX")"
corpus="$work/codegraph-rust"
viewer=""
cleanup() {
  if [ -n "$viewer" ]; then
    kill "$viewer" 2>/dev/null || true
    wait "$viewer" 2>/dev/null || true
  fi
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

CODEGRAPH_UI=1 "$bin" ui "$corpus" --no-open --port "$port" >"$work/viewer.log" 2>&1 &
viewer=$!
# shellcheck disable=SC2016 # $1 is expanded by the inner shell, which receives the port.
if ! timeout 30 sh -c 'until curl -sf "http://127.0.0.1:$1/api/stats" >/dev/null; do sleep 1; done' _ "$port"; then
  echo "error: the viewer did not answer on port $port; its output:" >&2
  cat "$work/viewer.log" >&2
  exit 1
fi

node "$here/capture-screens.mjs" "http://127.0.0.1:$port" "$out"
