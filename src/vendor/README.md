`urbdrc.rs` and `error.rs` are unmodified copies from IronRDP's `ironrdp-server`
crate (MIT OR Apache-2.0) at the commit pinned in `../../Cargo.toml`. That crate's
USB layer can only be reached through its own RDP server. Here it runs under the
daemon's event loop instead, which supplies the `crate::ServerEvent` it expects.
