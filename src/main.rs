//! Connects the URBDRC channels of an RDP session to `vhci_hcd`, so that a USB
//! device on the RDP client appears on this machine as a local device.
//!
//! The RDP server proxies the dynamic channels over a unix socket and knows nothing
//! about USB. Each connection to that socket is one RDP session. `docs/protocol.md`
//! describes the socket.

// The vendored modules are a general API, of which the daemon uses a part.
#[allow(dead_code)]
#[path = "vendor/error.rs"]
mod error;
#[allow(dead_code)]
#[path = "vendor/urbdrc.rs"]
mod urbdrc;

mod device;
mod proxy;
mod session;
mod usbip;
mod vhci;

use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;

use tokio::net::UnixListener;
use tracing_subscriber::EnvFilter;

/// The event type `urbdrc.rs` expects its crate to define.
pub enum ServerEvent {
    Usb(urbdrc::UrbdrcServerMessage),
}

const DEFAULT_SOCKET: &str = "/run/rdp-usb-gw/channel.sock";

#[tokio::main]
async fn main() -> std::io::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .without_time() // the journal adds its own
        .init();

    let path = std::env::args_os()
        .nth(1)
        .map_or_else(|| PathBuf::from(DEFAULT_SOCKET), PathBuf::from);
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let listener = UnixListener::bind(&path)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o660))?;
    tracing::info!(path = %path.display(), "Listening");

    loop {
        let (stream, _) = listener.accept().await?;
        tokio::spawn(async move {
            tracing::info!("RDP session connected");
            match session::run(stream).await {
                Ok(()) => tracing::info!("RDP session ended"),
                Err(error) => tracing::warn!(%error, "RDP session failed"),
            }
        });
    }
}
