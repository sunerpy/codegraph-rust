#!/usr/bin/env bash
# Fetch one W7-19a perf corpus at the commit the local copy came from and
# check it is the same source by its content digest (corpus-digest.sh,
# W7-19/a/perf/corpora-pins.txt). librechat and koel are `git archive`s of
# their commits; repo is `git archive` of codegraph-rust 2a600edd (in this
# checkout's history) without crates/codegraph-ui/viewer, as the W7-14 precision copy was made;
# feedbin the codeload tarball W7-16a fetched, checked by its sha256.
# Prints the spec perf.sh takes.
# usage: fetch-corpus.sh <corpus> <corpora-root>
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
corpus=$1
root=$2
dest=$root/$corpus
archive_of() { # url commit
  local git_dir=$root/.git-$corpus
  git init -q "$git_dir"
  git -C "$git_dir" fetch -q --depth 1 "$1" "$2"
  test "$(git -C "$git_dir" rev-parse FETCH_HEAD)" = "$2"
  mkdir -p "$dest" && git -C "$git_dir" archive FETCH_HEAD | tar -x -C "$dest"
  rm -rf "$git_dir"
}
case $corpus in
  librechat)
    archive_of https://github.com/danny-avila/LibreChat.git 738003b220a91ec724286dab0354080e22f8aac9
    want="files 2820 links 0 digest c06b4cb723b38ba4036c7b57bed65b775f94b63f27e06c1405c80bd416b6e863"
    spec="librechat:$dest:api/server/routes/convos.js:// :full" ;;
  repo)
    mkdir -p "$dest" && git archive 2a600eddd0d0f568cb98512b7caf94d17b44752c | tar -x -C "$dest"
    rm -rf -- "$dest/crates/codegraph-ui/viewer"
    want="files 1365 links 0 digest 92d83f102abd57ad2a5ffbc2322839cb118dcd0a3157c2ac3b5ad38bc391b050"
    spec="repo:$dest:crates/codegraph-core/src/lib.rs:// :full" ;;
  koel)
    archive_of https://github.com/koel/koel.git 8e7d1d0b2a079ccbfec1a10bd075aa79fcd16e06
    want="files 3174 links 3 digest b5214202bd19675424a4f7b962026d536ce9fb7f495b5360ef6155cb3ba7d89a"
    spec="koel:$dest:app/Services/SongService.php:// :sync" ;;
  feedbin)
    curl -sSfL -o "$root/feedbin.tar.gz" https://codeload.github.com/feedbin/feedbin/tar.gz/d6d569d72107e862c149d07b54ccdec9872c1f00
    test "$(sha256sum "$root/feedbin.tar.gz" | cut -d" " -f1)" = 48c5e174b1170e05e0a6e3cc131b51c9d47b6834f9229412872058e6c1ade806
    mkdir -p "$dest" && tar -xzf "$root/feedbin.tar.gz" -C "$dest" --strip-components=1 --no-same-owner
    rm -f "$root/feedbin.tar.gz"
    want="files 1669 links 0 digest e213db16ecb49586bc59183f59bd054b1aa83ad600fc84fa7cf9e91b4bfb69d0"
    spec="feedbin:$dest:app/controllers/entries_controller.rb:# :sync" ;;
  *) echo "unknown corpus $corpus" >&2; exit 2 ;;
esac
got=$(bash "$here/corpus-digest.sh" "$dest")
echo "$corpus: $got" >&2
test "$got" = "$want" || { echo "$corpus differs from the local copy: want $want" >&2; exit 1; }
du -sh "$dest" >&2
printf "%s\n" "$spec"
