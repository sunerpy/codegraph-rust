#!/usr/bin/env bash
# Fetch one W7-19c perf corpus at the commit the local copy came from and
# check it is the same source by its content digest (corpus-digest.sh).
# librechat and repo as W7-19a's and W7-19b's CI copies fetched them:
# librechat is a `git archive` of its commit; repo is `git archive` of
# codegraph-rust 2a600edd (in this checkout's history) without
# crates/codegraph-ui/viewer. mall, play-samples and guava are the codeload
# tarballs of the commits W7-19c's local copies came from
# (W7-19/c/corpora-fetch.log); the tarball's sha256 is logged and the
# content digest decides. play-samples-ctl is play-samples again, its sync
# edits a controller instead of a routes file.
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
  mall)
    tarball_of macrozheng/mall dcaa93b3150352e5044708d7211b5bed0af4509f
    want="files 717 links 0 digest f3b7bf29f78ec7a7c1aad15afcd35677360f6628b581c6c8d2260fb76791fc44"
    spec="mall:$dest:mall-admin/src/main/java/com/macro/mall/service/impl/CmsSubjectServiceImpl.java:// :full" ;;
  play-samples)
    tarball_of playframework/play-samples 518bb47a51a0340836db6b211722504eb82bd7a6
    want="files 867 links 0 digest 9a96782663f9f288a066852f99a2c3e1b6511f03156bf1a3fabecbef9ca7de2f"
    spec="play-samples:$dest:play-scala-hello-world-tutorial/conf/routes:# :full" ;;
  play-samples-ctl)
    tarball_of playframework/play-samples 518bb47a51a0340836db6b211722504eb82bd7a6
    want="files 867 links 0 digest 9a96782663f9f288a066852f99a2c3e1b6511f03156bf1a3fabecbef9ca7de2f"
    spec="play-samples-ctl:$dest:play-scala-hello-world-tutorial/app/controllers/HomeController.scala:// :sync" ;;
  guava)
    tarball_of google/guava ed0921b633d73d5a1d32c8c147784ca7add48757
    want="files 3365 links 0 digest 1c3440ed09f55981a11ef23286feace95e151c3d092b2f9345c4fcf8bef3cf94"
    spec="guava:$dest:guava/src/com/google/common/base/Ascii.java:// :full" ;;
  *) echo "unknown corpus $corpus" >&2; exit 2 ;;
esac
got=$(bash "$here/corpus-digest.sh" "$dest")
echo "$corpus: $got" >&2
test "$got" = "$want" || { echo "$corpus differs from the local copy: want $want" >&2; exit 1; }
du -sh "$dest" >&2
printf "%s\n" "$spec"
