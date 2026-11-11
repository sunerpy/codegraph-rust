#!/usr/bin/env bash
# Fetch one W7-20 perf corpus at the commit the local copy came from and
# check it is the same source by its content digest (corpus-digest.sh), or
# (typescript-large) by its tree.
# librechat and repo as W7-19a's to W7-19i's CI copies fetched them:
# librechat is a `git archive` of its commit; repo is `git archive` of
# codegraph-rust 2a600edd (in this checkout's history) without
# crates/codegraph-ui/viewer. koel, bookstack, spring-petclinic and the full
# ghostfolio tree are codeload tarballs of the pinned commits, unpacked as the
# local copies were (W7-16a, W7-19d, W7-19c, W7-20). typescript-large is a
# checkout of its commit with the tree checked, as W7-18b to W7-18d fetched it.
# koel-js and bookstack-js are koel and bookstack again; their syncs edit a
# client file instead of a PHP test.
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
want=
case $corpus in
  koel)
    tarball_of koel/koel 8e7d1d0b2a079ccbfec1a10bd075aa79fcd16e06
    want="files 3174 links 3 digest b5214202bd19675424a4f7b962026d536ce9fb7f495b5360ef6155cb3ba7d89a"
    spec="koel:$dest:tests/Feature/AuthTest.php:// :full" ;;
  koel-js)
    tarball_of koel/koel 8e7d1d0b2a079ccbfec1a10bd075aa79fcd16e06
    want="files 3174 links 3 digest b5214202bd19675424a4f7b962026d536ce9fb7f495b5360ef6155cb3ba7d89a"
    spec="koel-js:$dest:resources/assets/js/services/authService.ts:// :sync" ;;
  bookstack)
    tarball_of BookStackApp/BookStack 0b6b667e0d84ebb3406de6de9fadbf4230c29c0b
    want="files 2623 links 0 digest f42cfcd47fcd36a5c7f59570258832d22db79769393121af1dd35068ce3892ee"
    spec="bookstack:$dest:tests/Auth/RegistrationTest.php:// :full" ;;
  bookstack-js)
    tarball_of BookStackApp/BookStack 0b6b667e0d84ebb3406de6de9fadbf4230c29c0b
    want="files 2623 links 0 digest f42cfcd47fcd36a5c7f59570258832d22db79769393121af1dd35068ce3892ee"
    spec="bookstack-js:$dest:resources/js/components/attachments.js:// :sync" ;;
  spring-petclinic)
    tarball_of spring-projects/spring-petclinic 500158f732419217507c7656904b8e6aa1bcc0d6
    want="files 132 links 0 digest da0559a5813ed6825fd8bd9e0a3840cc2b1e1bbcd6a21055da8e9a784198a45a"
    spec="spring-petclinic:$dest:src/test/java/org/springframework/samples/petclinic/owner/OwnerControllerTests.java:// :full" ;;
  librechat)
    archive_of https://github.com/danny-avila/LibreChat.git 738003b220a91ec724286dab0354080e22f8aac9
    want="files 2820 links 0 digest c06b4cb723b38ba4036c7b57bed65b775f94b63f27e06c1405c80bd416b6e863"
    spec="librechat:$dest:api/server/routes/convos.js:// :full" ;;
  ghostfolio-full)
    tarball_of ghostfolio/ghostfolio 841ff112c4cbd775c5cbef85980057b1f4ecd61d
    want="files 1738 links 3 digest 17d3be9ce74fe3e49c8b5a891582176035e6c31ecbd0939ce9cc325df38eee8f"
    spec="ghostfolio-full:$dest:libs/ui/src/lib/services/data.service.ts:// :full" ;;
  repo)
    mkdir -p "$dest" && git archive 2a600eddd0d0f568cb98512b7caf94d17b44752c | tar -x -C "$dest"
    rm -rf -- "$dest/crates/codegraph-ui/viewer"
    want="files 1365 links 0 digest 92d83f102abd57ad2a5ffbc2322839cb118dcd0a3157c2ac3b5ad38bc391b050"
    spec="repo:$dest:crates/codegraph-core/src/lib.rs:// :full" ;;
  typescript-large)
    git init -q "$dest"
    git -C "$dest" fetch -q --depth 1 https://github.com/microsoft/TypeScript.git 7964e22f2b85f16e520f0e902c7fd7b6f0c15416
    git -C "$dest" checkout -q FETCH_HEAD
    test "$(git -C "$dest" rev-parse 'HEAD^{tree}')" = 743e0e967726bba4273315864ef78f8e17977f62
    spec="typescript-large:$dest:src/services/rename.ts:// :full" ;;
  *) echo "unknown corpus $corpus" >&2; exit 2 ;;
esac
if [ -n "$want" ]; then
  got=$(bash "$here/corpus-digest.sh" "$dest")
  echo "$corpus: $got" >&2
  test "$got" = "$want" || { echo "$corpus differs from the local copy: want $want" >&2; exit 1; }
fi
du -sh "$dest" >&2
printf "%s\n" "$spec"
