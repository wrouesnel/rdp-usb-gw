# Built from a tarball with the crates vendored (packaging/build-srpm.sh), since
# COPR builds offline.
%global debug_package %{nil}

Name:           rdp-usb-gw
Version:        0.2.0
Release:        1%{?dist}
Summary:        USB redirection for Linux RDP servers
License:        MIT OR Apache-2.0
URL:            https://github.com/wrouesnel/rdp-usb-gw
Source0:        %{name}_%{version}.orig.tar.xz
ExclusiveArch:  x86_64 aarch64

BuildRequires:  cargo >= 1.94
BuildRequires:  rust >= 1.94
BuildRequires:  systemd-rpm-macros
Requires:       kmod
%{?systemd_requires}

%description
rdp-usb-gw implements the server side of RDP USB redirection (MS-RDPEUSB, the
URBDRC dynamic channel). Devices an RDP client redirects, such as a security
key, appear on the Linux machine as local devices, attached through USB/IP's
vhci_hcd.

It doesn't speak RDP. The RDP server lends it the session's URBDRC channels
over a unix socket, so the server needs a small proxy for it, such as the one
in gnome-remote-desktop from the grd-with-usb-redirect packages.

EL10 kernels don't ship vhci_hcd; ELRepo's kmod-usbip provides it.

%prep
%autosetup -n %{name}-%{version}
mkdir -p .cargo
cat > .cargo/config.toml <<'CONFIG'
[source.crates-io]
replace-with = "vendored-sources"
[source."git+https://github.com/Devolutions/IronRDP?rev=da016ce"]
git = "https://github.com/Devolutions/IronRDP"
rev = "da016ce"
replace-with = "vendored-sources"
[source.vendored-sources]
directory = "vendor"
CONFIG

%build
cargo build --release --offline --frozen

%check
cargo test --release --offline --frozen

%install
install -D -m 0755 target/release/rdp-usb-gw %{buildroot}%{_bindir}/rdp-usb-gw
install -D -m 0644 data/rdp-usb-gw.service %{buildroot}%{_unitdir}/rdp-usb-gw.service
install -D -m 0644 data/70-rdp-usb-gw.rules %{buildroot}%{_udevrulesdir}/70-rdp-usb-gw.rules
install -D -m 0644 data/rdp-usb-gw.modules-load %{buildroot}%{_modulesloaddir}/rdp-usb-gw.conf
install -D -m 0644 data/rdp-usb-gw.sysusers %{buildroot}%{_sysusersdir}/rdp-usb-gw.conf

%post
systemd-sysusers %{_sysusersdir}/rdp-usb-gw.conf || :
udevadm control --reload-rules >/dev/null 2>&1 || :
%systemd_post rdp-usb-gw.service

%preun
%systemd_preun rdp-usb-gw.service

%postun
%systemd_postun_with_restart rdp-usb-gw.service

%files
%license LICENSE-APACHE LICENSE-MIT
%doc README.md docs/protocol.md
%{_bindir}/rdp-usb-gw
%{_unitdir}/rdp-usb-gw.service
%{_udevrulesdir}/70-rdp-usb-gw.rules
%{_modulesloaddir}/rdp-usb-gw.conf
%{_sysusersdir}/rdp-usb-gw.conf

%changelog
* Thu Nov 19 2026 Will Rouesnel <wrouesnel@wrouesnel.com> - 0.2.0-1
- First package.
