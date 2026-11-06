//! Hands one end of a socket to `vhci_hcd`, which then attaches a virtual device.

use std::os::fd::AsRawFd as _;
use std::os::unix::net::UnixStream;

use ironrdp_usb::UsbSpeed;

const DIR: &str = "/sys/devices/platform/vhci_hcd.0";
const PORT_FREE: u32 = 4; // VDEV_ST_NULL

/// Finds a free port on the USB 2 hub. RDPEUSB only announces full and high speed.
fn parse_free_port(status: &str) -> Option<u32> {
    status.lines().skip(1).find_map(|line| {
        let mut fields = line.split_whitespace();
        let hub = fields.next()?;
        let port = fields.next()?.parse().ok()?;
        let state = fields.next()?.parse::<u32>().ok()?;
        (hub == "hs" && state == PORT_FREE).then_some(port)
    })
}

/// The `usb_device_speed` values from the kernel's `ch9.h`.
fn kernel_speed(speed: UsbSpeed) -> u32 {
    match speed {
        UsbSpeed::Low => 1,
        UsbSpeed::Full => 2,
        _ => 3,
    }
}

pub fn attach(socket: &UnixStream, speed: UsbSpeed) -> std::io::Result<u32> {
    let status = std::fs::read_to_string(format!("{DIR}/status"))?;
    let port = parse_free_port(&status).ok_or_else(|| std::io::Error::other("no free vhci_hcd port"))?;
    // devid only has to be stable and non-zero: nothing else sees it.
    let devid = (1 << 16) | (port + 2);
    let request = format!("{port} {} {devid} {}", socket.as_raw_fd(), kernel_speed(speed));
    std::fs::write(format!("{DIR}/attach"), request)?;
    Ok(port)
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATUS: &str = "hub port sta spd dev      sockfd local_busid\n\
        hs  0000 006 002 00010002 000005 1-1\n\
        hs  0001 004 000 00000000 000000 0-0\n\
        ss  0008 004 000 00000000 000000 0-0\n";

    #[test]
    fn skips_busy_ports() {
        assert_eq!(parse_free_port(STATUS), Some(1));
    }

    #[test]
    fn ignores_the_superspeed_hub() {
        let busy = STATUS.replace("hs  0001 004", "hs  0001 006");
        assert_eq!(parse_free_port(&busy), None);
    }
}
