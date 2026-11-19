#!/usr/bin/env bash
# Builds in a container, on a glibc no newer than EL10's, because IronRDP needs a
# newer rustc than EL10 ships. Usage: build.sh [cargo args], default a release build.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
cache=${CARGO_CACHE:?set CARGO_CACHE to a directory for downloads and build output}
mkdir -p "$cache/registry" "$cache/git" "$cache/target"
args=${*:-build --release --locked}
podman run --rm -v "$here:/src:Z" -v "$cache/registry:/usr/local/cargo/registry:Z" \
    -v "$cache/git:/usr/local/cargo/git:Z" -v "$cache/target:/target:Z" \
    -e CARGO_TARGET_DIR=/target -w /src docker.io/library/rust:1-bookworm cargo $args
