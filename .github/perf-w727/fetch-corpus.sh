#!/usr/bin/env bash
# Fetch one W7-27 perf corpus at the commit the local copy came from and
# check it is the same source by its content digest (corpus-digest.sh), or
# (typescript-large) by its tree. librechat and typescript-large as W7-21's
# CI copy fetched them, koel as W7-20's, tokio-medium as W7-19h's (`tokio`
# there); django-realworld is the codeload tarball W7-19b's local copy came
# from (W7-19/b/corpora-fetch.log), redis-medium the bench corpus's commit.
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
tarball_of() { # owner/repo commit [member...]
  local repo=$1 sha=$2
  shift 2
  curl -sSfL --retry 3 -o "$root/$corpus.tar.gz" "https://codeload.github.com/$repo/tar.gz/$sha"
  echo "$corpus tarball sha256 $(sha256sum "$root/$corpus.tar.gz" | cut -d" " -f1)" >&2
  mkdir -p "$dest" && tar -xzf "$root/$corpus.tar.gz" -C "$dest" --strip-components=1 --no-same-owner --no-same-permissions "$@"
  rm -f "$root/$corpus.tar.gz"
}
want=
case $corpus in
  librechat)
    archive_of https://github.com/danny-avila/LibreChat.git 738003b220a91ec724286dab0354080e22f8aac9
    want="files 2820 links 0 digest c06b4cb723b38ba4036c7b57bed65b775f94b63f27e06c1405c80bd416b6e863"
    spec="librechat:$dest:api/server/routes/convos.js:// :full" ;;
  typescript-large)
    git init -q "$dest"
    git -C "$dest" fetch -q --depth 1 https://github.com/microsoft/TypeScript.git 7964e22f2b85f16e520f0e902c7fd7b6f0c15416
    git -C "$dest" checkout -q FETCH_HEAD
    test "$(git -C "$dest" rev-parse 'HEAD^{tree}')" = 743e0e967726bba4273315864ef78f8e17977f62
    spec="typescript-large:$dest:src/services/rename.ts:// :full" ;;
  koel)
    tarball_of koel/koel 8e7d1d0b2a079ccbfec1a10bd075aa79fcd16e06
    want="files 3174 links 3 digest b5214202bd19675424a4f7b962026d536ce9fb7f495b5360ef6155cb3ba7d89a"
    spec="koel:$dest:tests/Feature/AuthTest.php:// :full" ;;
  tokio-medium)
    archive_of https://github.com/tokio-rs/tokio.git ecb5125a6787b9d8eb818b1b00973bcd55ae77c0
    want="files 856 links 0 digest abc4a81f0dab868053a2125bd742fa796a294aed9525a659804530f2e0e48fce"
    spec="tokio-medium:$dest:tokio/src/sync/watch.rs:// :full" ;;
  django-realworld)
    tarball_of gothinkster/django-realworld-example-app 29c9d42831fa0cfc5a4aa7561f674396eacf20a2
    want="files 48 links 0 digest eca6d6597ab1db13421958f0714f3893ef4880c58f32a22189cc194714aaaa9f"
    spec="django-realworld:$dest:conduit/apps/articles/views.py:# :full" ;;
  redis-medium)
    archive_of https://github.com/redis/redis.git 498ecd0d6d007db11ddb3aea9428552598a78622
    want="files 1861 links 0 digest 68289b18f8ac7c5b32ec7a46feb5006b861dd75d7a03e9cc32fda30fc47df1d3"
    spec="redis-medium:$dest:src/t_hash.c:// :full" ;;
  *) echo "unknown corpus $corpus" >&2; exit 2 ;;
esac
if [ -n "$want" ]; then
  got=$(bash "$here/corpus-digest.sh" "$dest")
  echo "$corpus: $got" >&2
  test "$got" = "$want" || { echo "$corpus differs from the local copy: want $want" >&2; exit 1; }
fi
du -sh "$dest" >&2
printf "%s\n" "$spec"
