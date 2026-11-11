#!/usr/bin/env bash
# Fetch one W7-19h perf corpus at the commit the local copy came from and
# check it is the same source by its content digest (corpus-digest.sh).
# librechat and repo as W7-19a's to W7-19g's CI copies fetched them:
# librechat is a `git archive` of its commit; repo is `git archive` of
# codegraph-rust 2a600edd (in this checkout's history) without
# crates/codegraph-ui/viewer. tokio is a `git archive` of the commit
# bench/corpora/tokio-medium is checked out at; axum and actix-examples
# (W7-19/h/corpora-fetch.log) are the codeload tarballs of the commits the
# local copies came from. The tarball's sha256 is logged and the content
# digest decides. tokio-manifest and axum-manifest are tokio and axum
# again; their sync edits a member's Cargo.toml (the Rust control input:
# every Rust file re-resolved) instead of a source file.
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
    spec="repo:$dest:crates/codegraph-bench/fixtures/servers/axum/src/main.rs:// :full" ;;
  tokio)
    archive_of https://github.com/tokio-rs/tokio.git ecb5125a6787b9d8eb818b1b00973bcd55ae77c0
    want="files 856 links 0 digest abc4a81f0dab868053a2125bd742fa796a294aed9525a659804530f2e0e48fce"
    spec="tokio:$dest:tokio/src/sync/watch.rs:// :full" ;;
  tokio-manifest)
    archive_of https://github.com/tokio-rs/tokio.git ecb5125a6787b9d8eb818b1b00973bcd55ae77c0
    want="files 856 links 0 digest abc4a81f0dab868053a2125bd742fa796a294aed9525a659804530f2e0e48fce"
    spec="tokio-manifest:$dest:tokio-stream/Cargo.toml:# :sync" ;;
  axum)
    tarball_of tokio-rs/axum 90f299cb721bf930ba26c0f373b8b3d2e8295ec7
    want="files 500 links 5 digest 045956b044f553d740cbf187d01c2b2d3a68d1e0a56e5c0175c7e8d1b3f94905"
    spec="axum:$dest:axum/src/routing/tests/mod.rs:// :full" ;;
  axum-manifest)
    tarball_of tokio-rs/axum 90f299cb721bf930ba26c0f373b8b3d2e8295ec7
    want="files 500 links 5 digest 045956b044f553d740cbf187d01c2b2d3a68d1e0a56e5c0175c7e8d1b3f94905"
    spec="axum-manifest:$dest:axum-extra/Cargo.toml:# :sync" ;;
  actix-examples)
    tarball_of actix/examples 1ef1837ef02b52ad9ef936a3f148bbeb211c25a6
    want="files 473 links 5 digest f423ed2c077a1c8fd22e6839d5aa8da91cc6d9642d774dbc4dba79e70d3316ff"
    spec="actix-examples:$dest:websockets/chat/src/main.rs:// :full" ;;
  *) echo "unknown corpus $corpus" >&2; exit 2 ;;
esac
got=$(bash "$here/corpus-digest.sh" "$dest")
echo "$corpus: $got" >&2
test "$got" = "$want" || { echo "$corpus differs from the local copy: want $want" >&2; exit 1; }
du -sh "$dest" >&2
printf "%s\n" "$spec"
