#!/usr/bin/env bash
# Fetch one W7-19i perf corpus at the commit the local copy came from and
# check it is the same source by its content digest (corpus-digest.sh).
# librechat and repo as W7-19a's to W7-19h's CI copies fetched them:
# librechat is a `git archive` of its commit; repo is `git archive` of
# codegraph-rust 2a600edd (in this checkout's history) without
# crates/codegraph-ui/viewer. gin is a `git archive` of the commit W7-14c's
# sparse clone (no-cone `/*.go`, `/**/*.go`, `/go.mod`) checked out, pruned to
# those patterns; prometheus is a `git archive` of W7-15d's commit pruned to
# its sparse patterns (`/go.mod`, `/discovery/`); hotgo is the `server/` of a
# `git archive` of W7-16c's commit. gin-routes is gin again; its sync edits
# routes_test.go (64 routes) instead of context.go.
# Prints the spec perf.sh takes.
# usage: fetch-corpus.sh <corpus> <corpora-root>
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
corpus=$1
root=$2
dest=$root/$corpus
archive_of() { # url commit [subdir]
  local git_dir=$root/.git-$corpus
  git init -q "$git_dir"
  git -C "$git_dir" fetch -q --depth 1 "$1" "$2"
  test "$(git -C "$git_dir" rev-parse FETCH_HEAD)" = "$2"
  if [ -n "${3:-}" ]; then
    mkdir -p "$root/.sub-$corpus" && git -C "$git_dir" archive FETCH_HEAD "$3" | tar -x -C "$root/.sub-$corpus"
    mv "$root/.sub-$corpus/$3" "$dest" && rm -rf "$root/.sub-$corpus"
  else
    mkdir -p "$dest" && git -C "$git_dir" archive FETCH_HEAD | tar -x -C "$dest"
  fi
  rm -rf "$git_dir"
}
gin_sparse() { # keep the .go files at any depth and the root go.mod
  find "$dest" \( -type f -o -type l \) ! -name '*.go' ! -path "$dest/go.mod" -delete
  find "$dest" -mindepth 1 -type d -empty -delete
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
    spec="repo:$dest:crates/codegraph-bench/fixtures/servers/gin/main.go:// :full" ;;
  gin)
    archive_of https://github.com/gin-gonic/gin.git 0f09c3a9b4626d9fe9979cf4d0521d6ae32ab646
    gin_sparse
    want="files 100 links 0 digest 2447aeac5af9f88853aecd6c3de29d35fbed934698743fac5fc0cfabcd81b2c3"
    spec="gin:$dest:context.go:// :full" ;;
  gin-routes)
    archive_of https://github.com/gin-gonic/gin.git 0f09c3a9b4626d9fe9979cf4d0521d6ae32ab646
    gin_sparse
    want="files 100 links 0 digest 2447aeac5af9f88853aecd6c3de29d35fbed934698743fac5fc0cfabcd81b2c3"
    spec="gin-routes:$dest:routes_test.go:// :sync" ;;
  prometheus)
    archive_of https://github.com/prometheus/prometheus.git a61f1a95fec0422556464a4bfdfcd1e13bb9af62
    find "$dest" -mindepth 1 -maxdepth 1 ! -name discovery ! -name go.mod -exec rm -rf {} +
    want="files 226 links 0 digest 26652d3a0bee097b6bab01da71ed6d552a4204ab51f5ff6c5303713d36fe9f35"
    spec="prometheus:$dest:discovery/aws/ec2.go:// :full" ;;
  hotgo)
    archive_of https://github.com/bufanyun/hotgo.git 5b9b616b7c8c58b70fac5ff3affb0840f0146351 server
    want="files 823 links 0 digest dd1546bbd4202909488015cc5ecaf44d70b1937c04696412d56cfd93bc514b11"
    spec="hotgo:$dest:internal/router/websocket.go:// :full" ;;
  *) echo "unknown corpus $corpus" >&2; exit 2 ;;
esac
got=$(bash "$here/corpus-digest.sh" "$dest")
echo "$corpus: $got" >&2
test "$got" = "$want" || { echo "$corpus differs from the local copy: want $want" >&2; exit 1; }
du -sh "$dest" >&2
printf "%s\n" "$spec"
