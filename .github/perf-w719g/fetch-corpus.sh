#!/usr/bin/env bash
# Fetch one W7-19g perf corpus at the commit the local copy came from and
# check it is the same source by its content digest (corpus-digest.sh).
# librechat and repo as W7-19a's to W7-19f's CI copies fetched them:
# librechat is a `git archive` of its commit; repo is `git archive` of
# codegraph-rust 2a600edd (in this checkout's history) without
# crates/codegraph-ui/viewer. spi-server, codeedit and swift-nio
# (W7-19/g/corpora-fetch.log) are the codeload tarballs of the commits the
# local copies came from. The tarball's sha256 is logged and the content
# digest decides. spi-server-ctl is spi-server again; its sync edits a
# controller extension declaring route handlers instead of the routes file.
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
  spi-server)
    tarball_of SwiftPackageIndex/SwiftPackageIndex-Server 58b94786b5ecf36ad7def1bf8990aea43f076412
    want="files 978 links 0 digest 9126967358c47a9a93eea6152fbf3f21035edf790fac0599f116368fab1d9aab"
    spec="spi-server:$dest:Sources/App/routes.swift:// :full" ;;
  spi-server-ctl)
    tarball_of SwiftPackageIndex/SwiftPackageIndex-Server 58b94786b5ecf36ad7def1bf8990aea43f076412
    want="files 978 links 0 digest 9126967358c47a9a93eea6152fbf3f21035edf790fac0599f116368fab1d9aab"
    spec="spi-server-ctl:$dest:Sources/App/Controllers/PackageController+routes.swift:// :sync" ;;
  codeedit)
    tarball_of CodeEditApp/CodeEdit fa2aebd86373211c78626074b53ab75010767575
    want="files 874 links 0 digest 2ec902b0ce7da61816e558b439ae615f9a24bc87a50fca4cda341e864c324e75"
    spec="codeedit:$dest:CodeEdit/Features/Editor/Views/CodeFileView.swift:// :full" ;;
  swift-nio)
    tarball_of apple/swift-nio b440f7bae8894b5ec42e0f1842fcd77483ab3984
    want="files 764 links 2 digest 9d07152ec4e8fcbb6e44e96da48526108936323dad62ecfa169eaedb77055812"
    spec="swift-nio:$dest:Sources/NIOCore/Channel.swift:// :full" ;;
  *) echo "unknown corpus $corpus" >&2; exit 2 ;;
esac
got=$(bash "$here/corpus-digest.sh" "$dest")
echo "$corpus: $got" >&2
test "$got" = "$want" || { echo "$corpus differs from the local copy: want $want" >&2; exit 1; }
du -sh "$dest" >&2
printf "%s\n" "$spec"
