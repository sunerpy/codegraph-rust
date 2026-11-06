#!/usr/bin/env bash
# Fetch one corpus at the commit the local runs used and check it is the same
# source. ghostfolio is the W7-13 sparse clone: the commit's non-cone sparse
# checkout of the same seven patterns, checked by its content digest
# (corpus-digest.sh) against the local copy's. typescript-large is a
# checkout (the local copy is a clone; the tree is checked) and koel a `git
# archive` of the commit checked by its content digest, as W7-18b's CI copy
# fetched them. Prints the spec perf.sh takes.
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
  typescript-large)
    git init -q "$dest"
    git -C "$dest" fetch -q --depth 1 https://github.com/microsoft/TypeScript.git 7964e22f2b85f16e520f0e902c7fd7b6f0c15416
    git -C "$dest" checkout -q FETCH_HEAD
    test "$(git -C "$dest" rev-parse 'HEAD^{tree}')" = 743e0e967726bba4273315864ef78f8e17977f62
    spec="typescript-large:$dest:src/services/rename.ts:// " ;;
  koel)
    git_dir=$root/.git-$corpus
    git init -q "$git_dir"
    git -C "$git_dir" fetch -q --depth 1 https://github.com/koel/koel.git 8e7d1d0b2a079ccbfec1a10bd075aa79fcd16e06
    test "$(git -C "$git_dir" rev-parse FETCH_HEAD)" = 8e7d1d0b2a079ccbfec1a10bd075aa79fcd16e06
    mkdir -p "$dest" && git -C "$git_dir" archive FETCH_HEAD | tar -x -C "$dest"
    rm -rf "$git_dir"
    want="files 3174 links 3 digest b5214202bd19675424a4f7b962026d536ce9fb7f495b5360ef6155cb3ba7d89a"
    spec="koel:$dest:app/Services/SongService.php:// " ;;
  *) echo "unknown corpus $corpus" >&2; exit 2 ;;
esac
if [ -n "${want:-}" ]; then
  got=$(bash "$here/corpus-digest.sh" "$dest")
  echo "$corpus: $got" >&2
  test "$got" = "$want" || { echo "$corpus differs from the local copy: want $want" >&2; exit 1; }
fi
du -sh "$dest" >&2
printf '%s\n' "$spec"
