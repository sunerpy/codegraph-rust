#!/usr/bin/env bash
# Install the pinned actionlint release binary on a Linux x86_64 runner.
#
# taiki-e/install-action has no actionlint manifest, so the binary comes from
# the upstream GitHub release and is accepted only when its SHA-256 matches the
# digest pinned here. The version must equal ACTIONLINT_VERSION in the Makefile,
# which `make tools-check` enforces locally.
set -euo pipefail

version="1.7.12"
sha256="8aca8db96f1b94770f1b0d72b6dddcb1ebb8123cb3712530b08cc387b349a3d8"

makefile_version="$(sed -n 's/^ACTIONLINT_VERSION := //p' Makefile)"
if [ "$makefile_version" != "$version" ]; then
    printf 'install-actionlint: Makefile pins %s but this script pins %s\n' \
        "${makefile_version:-<none>}" "$version" >&2
    exit 1
fi

archive="actionlint_${version}_linux_amd64.tar.gz"
dest="${RUNNER_TEMP:?}/actionlint-${version}"
mkdir -p "$dest"
curl --proto '=https' --tlsv1.2 -fsSL --retry 3 -o "$dest/$archive" \
    "https://github.com/rhysd/actionlint/releases/download/v${version}/${archive}"
printf '%s  %s\n' "$sha256" "$dest/$archive" | sha256sum -c -
tar -xzf "$dest/$archive" -C "$dest" actionlint
"$dest/actionlint" -version
printf '%s\n' "$dest" >> "${GITHUB_PATH:?}"
