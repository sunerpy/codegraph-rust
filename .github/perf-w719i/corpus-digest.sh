#!/usr/bin/env bash
# The content digest of a corpus as the perf scripts copy it (rsync without
# .codegraph and .git): the sha256 of the sorted `sha256  ./path` lines of
# every regular file followed by the sorted `link ./path -> target` lines of
# every symlink, with the two counts.
# usage: corpus-digest.sh <dir>
set -euo pipefail
cd "$1"
files=$(find . \( -path ./.git -o -path ./.codegraph \) -prune -o -type f -print | LC_ALL=C sort)
links=$(find . \( -path ./.git -o -path ./.codegraph \) -prune -o -type l -print | LC_ALL=C sort)
digest=$({
  printf '%s\n' "$files" | tr '\n' '\0' | xargs -0 sha256sum
  if [ -n "$links" ]; then printf '%s\n' "$links" | while IFS= read -r l; do printf 'link %s -> %s\n' "$l" "$(readlink "$l")"; done; fi
} | sha256sum | cut -d" " -f1)
printf 'files %s links %s digest %s\n' "$(printf '%s\n' "$files" | grep -c .)" "$(printf '%s' "$links" | grep -c . || true)" "$digest"
