#!/usr/bin/env bash
# Fetch one W7-19f perf corpus at the commit the local copy came from and
# check it is the same source by its content digest (corpus-digest.sh).
# librechat and repo as W7-19a's to W7-19e's CI copies fetched them:
# librechat is a `git archive` of its commit; repo is `git archive` of
# codegraph-rust 2a600edd (in this checkout's history) without
# crates/codegraph-ui/viewer. clean-architecture (W7-16/a/corpora-fetch.log),
# ardalis-ca and eshoponweb (W7-19/f/corpora-fetch.log) are the codeload
# tarballs of the commits the local copies came from; newtonsoft is the
# codeload tarball of the commit bench/corpora/newtonsoft-large is checked
# out at (its digest checked against the tarball on 2026-11-10). The
# tarball's sha256 is logged and the content digest decides.
# eshoponweb-razor is eshoponweb again; its sync edits a Razor page (the
# probe lines are plain markup text) instead of a controller.
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
  clean-architecture)
    tarball_of jasontaylordev/CleanArchitecture 4739ae4cbd057ee142bc1e1bdc93c4fea8f0cac5
    want="files 257 links 0 digest 88c57973da3d8c48d4841166515b841adf57d28ee140e768371f503d33981419"
    spec="clean-architecture:$dest:src/Web/Endpoints/TodoItems.cs:// :full" ;;
  ardalis-ca)
    tarball_of ardalis/CleanArchitecture da5491f1641f3d388e6cfc2c0ddd29f763746762
    want="files 548 links 0 digest 8dde6adbb6266df7da9c49bf84f4e6f46a5ba29ae90730a839073fc00c81071c"
    spec="ardalis-ca:$dest:src/Clean.Architecture.Web/Contributors/Create.cs:// :full" ;;
  eshoponweb)
    tarball_of dotnet-architecture/eShopOnWeb 4da8212117e87d808d4bbc7da6286fd2147ce606
    want="files 459 links 0 digest 02e2a0ce142197584dec075db8c1dbb46a68b06e80d4a8b439904ae8597295f2"
    spec="eshoponweb:$dest:src/Web/Controllers/OrderController.cs:// :full" ;;
  eshoponweb-razor)
    tarball_of dotnet-architecture/eShopOnWeb 4da8212117e87d808d4bbc7da6286fd2147ce606
    want="files 459 links 0 digest 02e2a0ce142197584dec075db8c1dbb46a68b06e80d4a8b439904ae8597295f2"
    spec="eshoponweb-razor:$dest:src/Web/Pages/Basket/Index.cshtml::sync" ;;
  newtonsoft)
    tarball_of JamesNK/Newtonsoft.Json 4e13299d4b0ec96bd4df9954ef646bd2d1b5bf2a
    want="files 1169 links 0 digest 98aa9411b463d1cb6a9b85339c62ec245fa6432bb164ecce8845f033387135d6"
    spec="newtonsoft:$dest:Src/Newtonsoft.Json/Utilities/TypeExtensions.cs:// :full" ;;
  *) echo "unknown corpus $corpus" >&2; exit 2 ;;
esac
got=$(bash "$here/corpus-digest.sh" "$dest")
echo "$corpus: $got" >&2
test "$got" = "$want" || { echo "$corpus differs from the local copy: want $want" >&2; exit 1; }
du -sh "$dest" >&2
printf "%s\n" "$spec"
