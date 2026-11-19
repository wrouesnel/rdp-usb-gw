# The channel socket

rdp-usb-gw implements the server side of RDP USB redirection (MS-RDPEUSB, the `URBDRC`
dynamic virtual channel) and attaches each redirected device through USB/IP's `vhci_hcd`.
It doesn't speak RDP. An RDP server lends it the session's `URBDRC` channels over a unix
socket, through a small proxy that knows nothing about USB. This document is the contract
for writing that proxy, so any RDP server can offer USB redirection.

## Connecting

- rdp-usb-gw listens on `/run/rdp-usb-gw/channel.sock`, or the path given as its first
  argument. Anyone can connect, but rdp-usb-gw closes the connection at once unless the
  RDP server's process is root or in an allowed group: `rdp-usb-gw`, or the groups in
  `RDP_USB_GW_GROUPS`.
- The RDP server connects once per RDP session, when the session's dynamic channels are
  usable (after DRDYNVC is up). One connection is one session.
- If the socket isn't there, the server should carry on without USB redirection.
- Either side ends the session by closing the connection. rdp-usb-gw then detaches all of
  that session's devices.

## Frames

Every message in both directions is a frame: a 12-byte header and then the payload.

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 4 | type |
| 4 | 4 | channel |
| 8 | 4 | payload length, at most 16 MiB |
| 12 | length | payload |

The header fields are `uint32_t` in the host's native byte order, since both ends are on
the same machine.

`channel` is a number chosen by rdp-usb-gw. It names one instance of the `URBDRC` channel
within the session, and it isn't the DVC channel id on the wire.

| Type | Name | Direction | Payload | Meaning |
| --- | --- | --- | --- | --- |
| 1 | `OPEN` | rdp-usb-gw → server | empty | Open a new instance of the `URBDRC` dynamic channel and call it `channel`. |
| 2 | `OPENED` | server → rdp-usb-gw | `uint32_t` DVC channel id | The client accepted the channel; data can flow. |
| 3 | `DATA` | both | one channel message | One complete DVC message, reassembled from or to be split into DVC PDUs. |
| 4 | `CLOSE` | rdp-usb-gw → server | empty | Close the channel. |
| 5 | `CLOSED` | server → rdp-usb-gw | empty | The channel is closed, or couldn't be opened. |

## What the proxy does

- **On `OPEN`:** open a dynamic channel named `URBDRC` (with FreeRDP or WinPR,
  `WTSVirtualChannelOpenEx(session, "URBDRC", WTS_CHANNEL_OPTION_DYNAMIC)`).
  - If the client accepts it, send `OPENED`.
  - If the client refuses it, or doesn't answer within about 10 seconds, send `CLOSED`.
  - If the number is already in use, or the proxy is out of channels, send `CLOSED`.
    About 64 per session is plenty.
- **On data from the client on a channel:** send it as one `DATA` frame per message.
- **On `DATA` from rdp-usb-gw:** write the payload to that channel as one message.
  Drop it if the channel isn't open.
- **On `CLOSE`:** close the channel if it's open. Always answer with `CLOSED`.
- **When the client closes a channel:** send `CLOSED`.
- **On an unknown frame type or an oversized frame:** close the connection.

## The session

1. rdp-usb-gw sends `OPEN` for channel 1, the control channel. If the client has no USB
   redirection, the answer is `CLOSED` and rdp-usb-gw ends the session.
2. On `OPENED`, rdp-usb-gw runs MS-RDPEUSB's capability exchange over channel 1.
3. For each device the client offers, rdp-usb-gw sends `OPEN` for a new channel number,
   then talks to that device over it. When the device goes, it sends `CLOSE`, or the
   proxy reports `CLOSED`.

## Proxies

- **gnome-remote-desktop:** patches, packaged in
  [grd-with-usb-redirect](https://github.com/wrouesnel/grd-with-usb-redirect), that
  add a `usb-redirection-socket` setting (`grdctl rdp set-usb-redirection-socket`).
  The packaged builds default it to rdp-usb-gw's socket, including for remote login.
- **Other servers:** a FreeRDP-based server, such as KRdp or Weston's RDP backend, needs
  the same few hundred lines in-process. xrdp lets other programs open dynamic channels
  (`xrdpapi`), so an external proxy may be possible there without patching xrdp. That's
  untested.
