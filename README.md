# rdp-usb-gw

USB redirection for Linux RDP servers. An RDP client forwards a USB device, such as a
YubiKey, and it appears on the Linux machine as a local device.

rdp-usb-gw implements the server side of RDP USB redirection (MS-RDPEUSB, the `URBDRC`
dynamic channel) and attaches each device through USB/IP's `vhci_hcd`. Any client that
redirects USB over RDP should work. mstsc and the Remote Desktop ActiveX control are
tested.

It doesn't speak RDP itself. The RDP server lends it the session's `URBDRC` channels over
a unix socket, through a small proxy that knows nothing about USB.
[`docs/protocol.md`](docs/protocol.md) describes that socket. Writing a proxy is the only
work needed to support another RDP server.

| RDP server | Proxy |
| --- | --- |
| gnome-remote-desktop | A patch, packaged in [grd-with-usb-redirect](https://github.com/wrouesnel/grd-with-usb-redirect) |
| Others | Not written yet; see [`docs/protocol.md`](docs/protocol.md#proxies) |

## Contents

- `src/`: the daemon. `src/vendor/` holds two unmodified files from IronRDP.
- `data/`:
  - `rdp-usb-gw.service`: runs the daemon as root, with the socket owned by the
    `rdp-usb-gw` group.
  - `70-rdp-usb-gw.rules`: gives redirected HID devices to that group. A headless
    session has no seat, so `uaccess` would hand them to the greeter instead.
  - `rdp-usb-gw.sysusers` creates the group, and `rdp-usb-gw.modules-load` loads
    `vhci_hcd` at boot.

## Building

It needs Rust 1.94 or later, for IronRDP. `CARGO_CACHE=<dir> ./build.sh` builds a release
binary in a Debian bookworm container. Its glibc is old enough for EL10 and Ubuntu 24.04.
`./build.sh test` runs the tests.

## Installing

- Install `target/release/rdp-usb-gw` as `/usr/bin/rdp-usb-gw`.
- Install the files in `data/`:
  - `rdp-usb-gw.service` in `/usr/lib/systemd/system/`;
  - `70-rdp-usb-gw.rules` in `/usr/lib/udev/rules.d/`;
  - `rdp-usb-gw.sysusers` as `/usr/lib/sysusers.d/rdp-usb-gw.conf`;
  - `rdp-usb-gw.modules-load` as `/usr/lib/modules-load.d/rdp-usb-gw.conf`.
- Then:

  ```sh
  systemd-sysusers
  systemctl enable --now rdp-usb-gw
  usermod -aG rdp-usb-gw <the RDP server's user>
  ```

**`vhci_hcd`:** Ubuntu and Fedora kernels ship it; on Ubuntu it is in `linux-modules-extra`.
RHEL builds it but doesn't ship it. On EL10, ELRepo's
`kmod-usbip` provides it.

## Security

The daemon runs as root, because attaching a device means writing `vhci_hcd`'s sysfs
attach file. It parses only what arrives on its socket, which only the `rdp-usb-gw`
group can open. Every redirected device is one the RDP client chose to forward, and it
gets the same trust as a device plugged into the machine.

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at
your option. The files in `src/vendor/` are IronRDP's, under the same terms.
