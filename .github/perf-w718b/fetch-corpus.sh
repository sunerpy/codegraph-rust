#!/usr/bin/env bash
# Fetch one corpus at the commit the local runs used and check it is the same
# source: typescript-large and create-t3-turbo are checkouts (the local copies
# are clones; the tree is checked), koel and LibreChat are `git archive` of
# the commit (the local copies came from a codeload tarball and `git archive
# HEAD`). Every corpus but typescript-large is also checked by its content
# digest (corpus-digest.sh) against the local copy's. Prints the spec perf.sh
# takes.
# usage: fetch-corpus.sh <corpus> <corpora-root>
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
corpus=$1
root=$2
dest=$root/$corpus
git_dir=$root/.git-$corpus
fetch() { # url commit
  git init -q "$git_dir"
  git -C "$git_dir" fetch -q --depth 1 "$1" "$2"
  test "$(git -C "$git_dir" rev-parse FETCH_HEAD)" = "$2"
}
case $corpus in
  typescript-large)
    git init -q "$dest"
    git -C "$dest" fetch -q --depth 1 https://github.com/microsoft/TypeScript.git 7964e22f2b85f16e520f0e902c7fd7b6f0c15416
    git -C "$dest" checkout -q FETCH_HEAD
    test "$(git -C "$dest" rev-parse 'HEAD^{tree}')" = 743e0e967726bba4273315864ef78f8e17977f62
    spec="typescript-large:$dest:src/services/rename.ts:// " ;;
  koel)
    fetch https://github.com/koel/koel.git 8e7d1d0b2a079ccbfec1a10bd075aa79fcd16e06
    mkdir -p "$dest" && git -C "$git_dir" archive FETCH_HEAD | tar -x -C "$dest"
    want="files 3174 links 3 digest b5214202bd19675424a4f7b962026d536ce9fb7f495b5360ef6155cb3ba7d89a"
    spec="koel:$dest:app/Services/SongService.php:// " ;;
  librechat)
    fetch https://github.com/danny-avila/LibreChat.git 738003b220a91ec724286dab0354080e22f8aac9
    mkdir -p "$dest" && git -C "$git_dir" archive FETCH_HEAD | tar -x -C "$dest"
    want="files 2820 links 0 digest c06b4cb723b38ba4036c7b57bed65b775f94b63f27e06c1405c80bd416b6e863"
    spec="librechat:$dest:client/src/components/Nav/SettingsTabs/Data/SharedLinks.tsx:// " ;;
  t3)
    git init -q "$dest"
    git -C "$dest" fetch -q --depth 1 https://github.com/t3-oss/create-t3-turbo.git 8f945b7bb3bfb3ca8358d48b1ff0214079bc11ee
    git -C "$dest" checkout -q FETCH_HEAD
    test "$(git -C "$dest" rev-parse 'HEAD^{tree}')" = 7c1b9be7b78d474d67bc333ba3a889d5c457558e
    want="files 141 links 0 digest 68a6665c0d1c32b74403472073529b9a3043e13990be39fc6389a4b5c712a3d5"
    spec="t3:$dest:apps/nextjs/src/app/page.tsx:// " ;;
  *) echo "unknown corpus $corpus" >&2; exit 2 ;;
esac
if [ -n "${want:-}" ]; then
  got=$(bash "$here/corpus-digest.sh" "$dest")
  echo "$corpus: $got" >&2
  test "$got" = "$want" || { echo "$corpus differs from the local copy: want $want" >&2; exit 1; }
fi
rm -rf "$git_dir"
du -sh "$dest" >&2
printf '%s\n' "$spec"
