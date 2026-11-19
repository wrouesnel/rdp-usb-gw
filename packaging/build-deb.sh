#!/usr/bin/env bash
# Builds an Ubuntu source package for a PPA, with the crates vendored, since
# Launchpad builds offline.
#
# Usage: CARGO_CACHE=<dir> DEBEMAIL=<address> \
#          packaging/build-deb.sh <series> <output dir> [--binary]
#   <series> is noble or resolute. --binary also builds the .deb, to test.
#
# The orig tarball is made once per version and reused, so every series'
# upload carries the same one. The source package is unsigned; sign it with
# debsign before dput.
set -euo pipefail
here=$(cd "$(dirname "$0")/.." && pwd)
series=$1; out=$(mkdir -p "$2" && cd "$2" && pwd); binary=${3:-}
cache=${CARGO_CACHE:?set CARGO_CACHE to a directory for crate downloads}
: "${DEBEMAIL:?set DEBEMAIL to the uploader email}"
case $series in
    noble) release=24.04 ;;
    resolute) release=26.04 ;;
    *) echo "unknown series $series" >&2; exit 2 ;;
esac
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$here/Cargo.toml" | head -1)
revision=${REVISION:-1}
orig=$out/rdp-usb-gw_$version.orig.tar.xz

if [ ! -e "$orig" ]; then
    work=$(mktemp -d); trap 'rm -rf "$work"' EXIT
    git -C "$here" archive --prefix="rdp-usb-gw-$version/" HEAD -- . ':!debian' | tar -x -C "$work"
    mkdir -p "$cache/registry" "$cache/git"
    podman run --rm -v "$work/rdp-usb-gw-$version:/src:Z" \
        -v "$cache/registry:/usr/local/cargo/registry:Z" -v "$cache/git:/usr/local/cargo/git:Z" \
        -w /src docker.io/library/rust:1-bookworm \
        sh -c 'cargo vendor --locked --versioned-dirs vendor >/dev/null'
    tar -C "$work" --sort=name --owner=0 --group=0 --numeric-owner \
        --mtime="@$(git -C "$here" log -1 --format=%ct)" \
        -cJf "$orig" "rdp-usb-gw-$version"
fi
[ -z "${ORIG_ONLY:-}" ] || exit 0

podman run --rm -i -v "$here/debian:/debian:ro,Z" -v "$out:/out:Z" \
    -e VERSION="$version" -e DEBVER="$version-$revision~$release.1" -e SERIES="$series" \
    -e DEBEMAIL="$DEBEMAIL" -e DEBFULLNAME="${DEBFULLNAME:-Will Rouesnel}" -e BINARY="$binary" \
    "docker.io/library/ubuntu:$series" bash -euo pipefail <<'SCRIPT'
export DEBIAN_FRONTEND=noninteractive
echo 'Acquire::Retries "10"; Acquire::http::Timeout "30";' > /etc/apt/apt.conf.d/80retries
apt-get -qq update
apt-get -qq install -y --no-install-recommends dpkg-dev devscripts debhelper fakeroot >/dev/null
mkdir /build && cd /build
cp /out/rdp-usb-gw_"$VERSION".orig.tar.xz .
tar -xf rdp-usb-gw_"$VERSION".orig.tar.xz
cd rdp-usb-gw-"$VERSION"
cp -r /debian debian
dch --create --package rdp-usb-gw -v "$DEBVER" -D "$SERIES" --force-distribution \
    "Build for Ubuntu $SERIES."
dpkg-buildpackage -S -sa -d -us -uc >/out/rdp-usb-gw-source.log 2>&1 || { tail -30 /out/rdp-usb-gw-source.log; exit 1; }
cp ../*.dsc ../*.debian.tar.* ../*_source.changes ../*_source.buildinfo /out/
if [ -n "$BINARY" ]; then
    apt-get -qq build-dep -y ./ >/dev/null
    dpkg-buildpackage -b -us -uc >/out/rdp-usb-gw-binary.log 2>&1 || { tail -60 /out/rdp-usb-gw-binary.log; exit 1; }
    cp ../*.deb /out/
fi
SCRIPT
ls "$out"
