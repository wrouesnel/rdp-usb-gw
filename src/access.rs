//! Decides which RDP servers may use the socket.
//!
//! The socket itself is open to every local user, so that an admin can let in
//! RDP servers that can't join a fixed group, such as the login screen's, whose
//! users GDM creates on demand. Each connection is checked instead: the
//! connecting process must be root, or have one of the allowed groups as its
//! primary or a supplementary group.

use std::collections::HashSet;

use tokio::net::UnixStream;

/// Groups allowed by default: the RDP servers of user sessions join it.
pub const DEFAULT_GROUPS: &str = "rdp-usb-gw";

/// The environment variable that replaces [`DEFAULT_GROUPS`]: group names or
/// numbers, separated by spaces or commas.
pub const GROUPS_VARIABLE: &str = "RDP_USB_GW_GROUPS";

pub struct Access {
    gids: HashSet<u32>,
}

impl Access {
    /// Reads the allowed groups from [`GROUPS_VARIABLE`], or the default.
    pub fn from_env() -> Self {
        let spec = std::env::var(GROUPS_VARIABLE).unwrap_or_else(|_| DEFAULT_GROUPS.to_owned());
        let groups = std::fs::read_to_string("/etc/group").unwrap_or_default();
        let mut gids = HashSet::new();
        for name in spec.split([' ', ',']).filter(|n| !n.is_empty()) {
            match resolve_group(&groups, name) {
                Some(gid) => {
                    tracing::info!(group = name, gid, "Allowing group");
                    gids.insert(gid);
                }
                None => tracing::warn!(group = name, code = "RUG-ACCESS-01", "Unknown group, ignored"),
            }
        }
        Self { gids }
    }

    /// Whether the process at the other end of `stream` may use the socket.
    pub fn allows(&self, stream: &UnixStream) -> bool {
        let cred = match stream.peer_cred() {
            Ok(cred) => cred,
            Err(error) => {
                tracing::warn!(%error, code = "RUG-ACCESS-02", "Can't read the peer's credentials, refusing");
                return false;
            }
        };
        if cred.uid() == 0 || self.gids.contains(&cred.gid()) {
            return true;
        }
        // The kernel reports only the primary group; the others come from /proc,
        // which stays valid while the peer holds the connection open.
        let supplementary = cred.pid().and_then(|pid| supplementary_groups(pid as u32));
        if supplementary.is_some_and(|groups| groups.iter().any(|g| self.gids.contains(g))) {
            return true;
        }
        tracing::warn!(
            uid = cred.uid(),
            gid = cred.gid(),
            pid = cred.pid(),
            code = "RUG-ACCESS-03",
            "Refused a connection from a user outside the allowed groups ({GROUPS_VARIABLE})"
        );
        false
    }
}

/// A group's number, from its name in an `/etc/group` file, or the number itself.
fn resolve_group(groups: &str, name: &str) -> Option<u32> {
    if let Ok(gid) = name.parse() {
        return Some(gid);
    }
    groups.lines().find_map(|line| {
        let mut fields = line.split(':');
        (fields.next() == Some(name)).then(|| fields.nth(1)?.parse().ok()).flatten()
    })
}

/// The supplementary groups of process `pid`.
fn supplementary_groups(pid: u32) -> Option<Vec<u32>> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let line = status.lines().find_map(|l| l.strip_prefix("Groups:"))?;
    Some(line.split_whitespace().filter_map(|g| g.parse().ok()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const GROUPS: &str = "root:x:0:\ngdm:x:979:\nrdp-usb-gw:x:978:alice\n";

    #[test]
    fn resolves_names_and_numbers() {
        assert_eq!(resolve_group(GROUPS, "gdm"), Some(979));
        assert_eq!(resolve_group(GROUPS, "rdp-usb-gw"), Some(978));
        assert_eq!(resolve_group(GROUPS, "1234"), Some(1234));
        assert_eq!(resolve_group(GROUPS, "nobody-here"), None);
        assert_eq!(resolve_group(GROUPS, "gd"), None);
    }

    #[tokio::test]
    async fn allows_its_own_groups() {
        let (a, _b) = UnixStream::pair().unwrap();
        let cred = a.peer_cred().unwrap();
        let own = Access { gids: HashSet::from([cred.gid()]) };
        assert!(own.allows(&a));
        let none = Access { gids: HashSet::new() };
        assert_eq!(none.allows(&a), cred.uid() == 0);
    }
}
