#!/usr/bin/env bash
# Fetch one W7-19b perf corpus at the commit the local copy came from and
# check it is the same source by its content digest (corpus-digest.sh).
# librechat and repo as W7-19a's CI copy fetched them: librechat is a
# `git archive` of its commit; repo is `git archive` of codegraph-rust
# 2a600edd (in this checkout's history) without crates/codegraph-ui/viewer.
# netbox, redash, flaskbb and fastapi-template are the codeload tarballs of
# the commits W7-19b's precision copies came from (W7-19/b/corpora-fetch.log);
# the tarball's sha256 is logged and the content digest decides.
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
  curl -sSfL -o "$root/$corpus.tar.gz" "https://codeload.github.com/$1/tar.gz/$2"
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
  netbox)
    tarball_of netbox-community/netbox 40e422c84d63192d4646e77fd506e6fde48c4030
    want="files 2315 links 0 digest 45a24bb7f01f37498e5093d7cf69a5facb5fa53f50d15610def5a43b0800bdae"
    spec="netbox:$dest:netbox/dcim/urls.py:# :full" ;;
  redash)
    tarball_of getredash/redash 09740eaf6359af4ac4f932b620f28add2299142e
    want="files 1976 links 0 digest 8d1ad02da59b898008d0c98291f1e03acd2e37186342fc2c91ab79d6b2ebc48d"
    spec="redash:$dest:redash/handlers/api.py:# :sync" ;;
  flaskbb)
    tarball_of flaskbb/flaskbb 1a649a915984f8ebe6e6199d5a449b777ac451d9
    want="files 508 links 1 digest 5154074b77f6a257410ad8c055c0db70f09db05dd9e968ef973568fdc8cd116a"
    spec="flaskbb:$dest:flaskbb/forum/views.py:# :sync" ;;
  fastapi-template)
    tarball_of fastapi/full-stack-fastapi-template aabb90126964a164e02aff39e7c385bb0598236d
    want="files 248 links 4 digest 40ab158e5d9503afeb78d50729d22a3a760151c69c3eae96b7a71b93edc67249"
    spec="fastapi-template:$dest:backend/app/api/routes/items.py:# :sync" ;;
  *) echo "unknown corpus $corpus" >&2; exit 2 ;;
esac
got=$(bash "$here/corpus-digest.sh" "$dest")
echo "$corpus: $got" >&2
test "$got" = "$want" || { echo "$corpus differs from the local copy: want $want" >&2; exit 1; }
du -sh "$dest" >&2
printf "%s\n" "$spec"
