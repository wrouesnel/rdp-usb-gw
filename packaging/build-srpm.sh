#!/usr/bin/env bash
# Builds the EL10 source RPM for COPR, from the vendored tarball that
# build-deb.sh makes (it makes one if there isn't one).
#
# Usage: CARGO_CACHE=<dir> packaging/build-srpm.sh <output dir> [--binary]
#   --binary also builds the RPM, to test.
set -euo pipefail
here=$(cd "$(dirname "$0")/.." && pwd)
out=$(mkdir -p "$1" && cd "$1" && pwd); binary=${2:-}
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$here/Cargo.toml" | head -1)
orig=$out/rdp-usb-gw_$version.orig.tar.xz
if [ ! -e "$orig" ]; then
    DEBEMAIL=unused ORIG_ONLY=1 "$here/packaging/build-deb.sh" noble "$out"
fi
podman run --rm -i -v "$here/packaging:/packaging:ro,Z" -v "$out:/out:Z" -e BINARY="$binary" \
    docker.io/library/almalinux:10 bash -euo pipefail <<'SCRIPT'
dnf -y -q install rpm-build rpmdevtools systemd-rpm-macros >/dev/null
mkdir -p ~/rpmbuild/SOURCES ~/rpmbuild/SPECS
cp /out/rdp-usb-gw_*.orig.tar.xz ~/rpmbuild/SOURCES/
cp /packaging/rdp-usb-gw.spec ~/rpmbuild/SPECS/
cd ~/rpmbuild/SPECS
rpmbuild -bs rdp-usb-gw.spec >/dev/null
cp ~/rpmbuild/SRPMS/*.src.rpm /out/
if [ -n "$BINARY" ]; then
    dnf -y -q install cargo rust >/dev/null
    rpmbuild -bb rdp-usb-gw.spec > /out/rdp-usb-gw-rpm.log 2>&1 || { tail -60 /out/rdp-usb-gw-rpm.log; exit 1; }
    cp ~/rpmbuild/RPMS/*/*.rpm /out/
fi
SCRIPT
ls "$out"
