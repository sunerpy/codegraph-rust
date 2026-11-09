#!/usr/bin/env bash
# Fetch one W7-19d perf corpus at the commit the local copy came from and
# check it is the same source by its content digest (corpus-digest.sh).
# librechat and repo as W7-19a's to W7-19c's CI copies fetched them:
# librechat is a `git archive` of its commit; repo is `git archive` of
# codegraph-rust 2a600edd (in this checkout's history) without
# crates/codegraph-ui/viewer. koel (W7-16/a/corpora-fetch.log), akaunting and
# drupal (W7-19/d/corpora-fetch.log) are the codeload tarballs of the commits
# the local copies came from; the tarball's sha256 is logged and the content
# digest decides. koel-ctl is koel again, its sync edits a controller instead
# of a routes file; drupal-mod is drupal again, its sync edits a `.module`
# hook file instead of a routing file.
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
  koel)
    tarball_of koel/koel 8e7d1d0b2a079ccbfec1a10bd075aa79fcd16e06
    want="files 3174 links 3 digest b5214202bd19675424a4f7b962026d536ce9fb7f495b5360ef6155cb3ba7d89a"
    spec="koel:$dest:routes/api.base.php:// :full" ;;
  koel-ctl)
    tarball_of koel/koel 8e7d1d0b2a079ccbfec1a10bd075aa79fcd16e06
    want="files 3174 links 3 digest b5214202bd19675424a4f7b962026d536ce9fb7f495b5360ef6155cb3ba7d89a"
    spec="koel-ctl:$dest:app/Http/Controllers/API/SongController.php:// :sync" ;;
  akaunting)
    tarball_of akaunting/akaunting d0bb0f2b269ed9e1f1a763fb93f17fb1a3af06ce
    want="files 4355 links 0 digest c1685931c0e32093bd2e6034a03c2287b81bac77d8b8f8d5dc47540450fe60f3"
    spec="akaunting:$dest:app/Http/Controllers/Sales/Invoices.php:// :full" ;;
  drupal)
    tarball_of drupal/drupal 9b26fc7ce1d590d3d6e0b4f0f59ed7594bf83ae7
    want="files 8135 links 0 digest 02f389d7a2f3c4e4716df7c8085ac7ee93cb40dec6953b2d3333915105c23d89"
    spec="drupal:$dest:core/modules/node/node.routing.yml:# :full" ;;
  drupal-mod)
    tarball_of drupal/drupal 9b26fc7ce1d590d3d6e0b4f0f59ed7594bf83ae7
    want="files 8135 links 0 digest 02f389d7a2f3c4e4716df7c8085ac7ee93cb40dec6953b2d3333915105c23d89"
    spec="drupal-mod:$dest:core/modules/node/node.module:// :sync" ;;
  *) echo "unknown corpus $corpus" >&2; exit 2 ;;
esac
got=$(bash "$here/corpus-digest.sh" "$dest")
echo "$corpus: $got" >&2
test "$got" = "$want" || { echo "$corpus differs from the local copy: want $want" >&2; exit 1; }
du -sh "$dest" >&2
printf "%s\n" "$spec"
