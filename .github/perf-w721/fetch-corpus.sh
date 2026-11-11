#!/usr/bin/env bash
# Fetch one W7-21 perf corpus at the commit the local copy came from and
# check it is the same source by its content digest (corpus-digest.sh), or
# (typescript-large) by its tree. The React Native libraries and
# wikipedia-ios are codeload tarballs of their pinned commits, unpacked as
# the local copies were (W7-21/corpora-fetch.log); expo-subset is the
# packages/expo-camera and packages/expo-haptics of expo/expo's tarball.
# librechat and typescript-large as W7-20's CI copy fetched them. The
# `-m`, `-alias` and `-objc` corpora are the same source again; their syncs
# edit another file.
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
FIREBASE="files 2551 links 0 digest 0448db5d5849ea1d1637633c70b557540caaabedd371bf0b045785783cf2dd03"
WIKI="files 4725 links 4 digest c078107ef368a467fad473bf051ef0ded290794e709bdf8f596d9bc3b2a6429f"
want=
case $corpus in
  rn-firebase)
    tarball_of invertase/react-native-firebase fbf1ada57475bb8c99c6340cac69f02fbb08d4bf
    want=$FIREBASE
    spec="rn-firebase:$dest:packages/app/e2e/app.e2e.js:// :full" ;;
  rn-firebase-m)
    tarball_of invertase/react-native-firebase fbf1ada57475bb8c99c6340cac69f02fbb08d4bf
    want=$FIREBASE
    spec="rn-firebase-m:$dest:tests/ios/testing/RNFBTestingMessagingModule.m:// :sync" ;;
  rn-firebase-alias)
    tarball_of invertase/react-native-firebase fbf1ada57475bb8c99c6340cac69f02fbb08d4bf
    want=$FIREBASE
    spec="rn-firebase-alias:$dest:packages/app/e2e/events.e2e.js:// :sync" ;;
  rn-screens)
    tarball_of software-mansion/react-native-screens 178ae26d09b5cde197770ea0040b42daf516ddf3
    want="files 2284 links 0 digest 40f38bcba82a7720dbf644d9740e68358d332afddae631b630e45fd27029b264"
    spec="rn-screens:$dest:src/index.tsx:// :full" ;;
  rn-svg)
    tarball_of software-mansion/react-native-svg 4fd7551d48e38964dd6d2941f9971b3c197478d7
    want="files 1048 links 2 digest f8ea71da3d72d1cfa2d8baae33b2dfba329a9c2c522666a6cd50e2c96b0de235"
    spec="rn-svg:$dest:src/index.ts:// :full" ;;
  wikipedia-ios)
    tarball_of wikimedia/wikipedia-ios ee61b71779699839e3037dd9b2895087e0999bc5
    want=$WIKI
    spec="wikipedia-ios:$dest:Wikipedia/Code/WMFAppViewController.swift:// :full" ;;
  wikipedia-ios-objc)
    tarball_of wikimedia/wikipedia-ios ee61b71779699839e3037dd9b2895087e0999bc5
    want=$WIKI
    spec="wikipedia-ios-objc:$dest:Wikipedia/Code/MWKDataStore.m:// :sync" ;;
  expo-subset)
    sha=23efdd46c6d969adb7fe2f50aa34a96d218569c0
    tarball_of expo/expo "$sha" "expo-$sha/packages/expo-camera" "expo-$sha/packages/expo-haptics"
    want="files 128 links 0 digest 37157bf41180fb4b66c968ee65f1e4a20642a9c5e56a645dd78217e845ea138b"
    spec="expo-subset:$dest:packages/expo-haptics/android/src/main/java/expo/modules/haptics/HapticsModule.kt:// :full" ;;
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
  *) echo "unknown corpus $corpus" >&2; exit 2 ;;
esac
if [ -n "$want" ]; then
  got=$(bash "$here/corpus-digest.sh" "$dest")
  echo "$corpus: $got" >&2
  test "$got" = "$want" || { echo "$corpus differs from the local copy: want $want" >&2; exit 1; }
fi
du -sh "$dest" >&2
printf "%s\n" "$spec"
