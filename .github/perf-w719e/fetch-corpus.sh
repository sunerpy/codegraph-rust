#!/usr/bin/env bash
# Fetch one W7-19e perf corpus at the commit the local copy came from and
# check it is the same source by its content digest (corpus-digest.sh).
# librechat and repo as W7-19a's to W7-19d's CI copies fetched them:
# librechat is a `git archive` of its commit; repo is `git archive` of
# codegraph-rust 2a600edd (in this checkout's history) without
# crates/codegraph-ui/viewer. feedbin (W7-16/a/corpora-fetch.log), maybe and
# solidus (W7-19/e/corpora-fetch.log) are the codeload tarballs of the
# commits the local copies came from; the tarball's sha256 is logged and the
# content digest decides. feedbin-ctl is feedbin again, its sync edits a
# controller instead of the routes file.
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
tarball_of() { # owner/repo commit
  curl -sSfL --retry 3 -o "$root/$corpus.tar.gz" "https://codeload.github.com/$1/tar.gz/$2"
  echo "$corpus tarball sha256 $(sha256sum "$root/$corpus.tar.gz" | cut -d" " -f1)" >&2
  mkdir -p "$dest" && tar -xzf "$root/$corpus.tar.gz" -C "$dest" --strip-components=1 --no-same-owner --no-same-permissions
  rm -f "$root/$corpus.tar.gz"
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
  feedbin)
    tarball_of feedbin/feedbin d6d569d72107e862c149d07b54ccdec9872c1f00
    want="files 1669 links 0 digest e213db16ecb49586bc59183f59bd054b1aa83ad600fc84fa7cf9e91b4bfb69d0"
    spec="feedbin:$dest:config/routes.rb:# :full" ;;
  feedbin-ctl)
    tarball_of feedbin/feedbin d6d569d72107e862c149d07b54ccdec9872c1f00
    want="files 1669 links 0 digest e213db16ecb49586bc59183f59bd054b1aa83ad600fc84fa7cf9e91b4bfb69d0"
    spec="feedbin-ctl:$dest:app/controllers/entries_controller.rb:# :sync" ;;
  maybe)
    tarball_of maybe-finance/maybe 77b5469832758d1cbee1a940f3012a1ae1c74cd3
    want="files 1644 links 0 digest 890ef480880251369bfd9e22d41750ef2c68f49558f4c833b0cf17dddd53dcab"
    spec="maybe:$dest:config/routes.rb:# :full" ;;
  solidus)
    tarball_of solidusio/solidus 6cbf0aa8cc4bb9a1fe31d3d73860eb6f910e7922
    want="files 3808 links 0 digest a28184bb65e9465417a039f017e0db4406ff01e6a4208bee6b730c715988c0a4"
    spec="solidus:$dest:backend/config/routes.rb:# :full" ;;
  *) echo "unknown corpus $corpus" >&2; exit 2 ;;
esac
got=$(bash "$here/corpus-digest.sh" "$dest")
echo "$corpus: $got" >&2
test "$got" = "$want" || { echo "$corpus differs from the local copy: want $want" >&2; exit 1; }
du -sh "$dest" >&2
printf "%s\n" "$spec"
