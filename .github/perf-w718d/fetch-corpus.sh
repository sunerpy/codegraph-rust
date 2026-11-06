#!/usr/bin/env bash
# Fetch one corpus at the commit the local runs used and check it is the same
# source. ghostfolio is the W7-13 sparse clone: the commit's non-cone sparse
# checkout of the same seven patterns, checked by its content digest
# (corpus-digest.sh) against the local copy's. Prints the spec perf.sh takes.
# usage: fetch-corpus.sh <corpus> <corpora-root>
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
corpus=$1
root=$2
dest=$root/$corpus
case $corpus in
  ghostfolio)
    git init -q "$dest"
    git -C "$dest" fetch -q --depth 1 https://github.com/ghostfolio/ghostfolio.git 841ff112c4cbd775c5cbef85980057b1f4ecd61d
    test "$(git -C "$dest" rev-parse 'FETCH_HEAD^{tree}')" = bbbb2cbc0030d64cd37b35ca7c18e4348320dc2d
    git -C "$dest" sparse-checkout set --no-cone '/apps/client/src/app/' '/libs/common/src/lib/routes/' '/libs/ui/src/lib/' '/package.json' '/angular.json' '/nx.json' '/tsconfig*.json'
    git -C "$dest" checkout -q FETCH_HEAD
    want="files 636 links 0 digest f293399b60baec13f029766a2a7bd5ff373f5abbfaab922279328545b2ee24e4"
    spec="ghostfolio:$dest:apps/client/src/app/components/header/header.component.ts:// " ;;
  *) echo "unknown corpus $corpus" >&2; exit 2 ;;
esac
got=$(bash "$here/corpus-digest.sh" "$dest")
echo "$corpus: $got" >&2
test "$got" = "$want" || { echo "$corpus differs from the local copy: want $want" >&2; exit 1; }
du -sh "$dest" >&2
printf '%s\n' "$spec"
