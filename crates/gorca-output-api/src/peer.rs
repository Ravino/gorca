// SPDX-License-Identifier: GPL-3.0-or-later
//! Who is on the other end of a speech socket (review F14).
//!
//! gorca sends everything it reads to the speech service, passwords and terminal output
//! included, so it must only talk to a peer running as the same user. The check is pure
//! (`peer_uid` against `own_uid`) so it is testable without a second user.

use std::os::unix::fs::MetadataExt;

/// The effective uid of this process, from the owner of `/proc/self` (std only, no `unsafe`).
pub fn own_uid() -> Option<u32> {
    std::fs::metadata("/proc/self").ok().map(|m| m.uid())
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum PeerError {
    #[error("the speech socket is served by uid {peer}, not by us (uid {own})")]
    Other { peer: u32, own: u32 },
    #[error("cannot determine our own uid")]
    UnknownSelf,
}

/// `Ok` when `peer_uid` is `own`.
pub fn check_peer(peer_uid: u32, own: Option<u32>) -> Result<(), PeerError> {
    match own {
        Some(o) if o == peer_uid => Ok(()),
        Some(own) => Err(PeerError::Other {
            peer: peer_uid,
            own,
        }),
        None => Err(PeerError::UnknownSelf),
    }
}

/// [`check_peer`] with an explicit allow-list: a peer whose uid is in `allowed` (a system-wide
/// voicego or speech-dispatcher running as its own service user, for example) passes too. The
/// default, an empty list, keeps the same-user rule.
pub fn check_peer_allowed(
    peer_uid: u32,
    own: Option<u32>,
    allowed: &[u32],
) -> Result<(), PeerError> {
    if allowed.contains(&peer_uid) {
        return Ok(());
    }
    check_peer(peer_uid, own)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_allow_list_admits_listed_foreign_uids_only() {
        assert_eq!(check_peer_allowed(0, Some(1000), &[0]), Ok(()));
        assert_eq!(check_peer_allowed(1000, Some(1000), &[]), Ok(()));
        assert_eq!(
            check_peer_allowed(0, Some(1000), &[]),
            Err(PeerError::Other { peer: 0, own: 1000 })
        );
        assert!(check_peer_allowed(115, Some(1000), &[0]).is_err());
    }

    #[test]
    fn only_our_own_uid_passes() {
        assert_eq!(check_peer(1000, Some(1000)), Ok(()));
        assert_eq!(
            check_peer(0, Some(1000)),
            Err(PeerError::Other { peer: 0, own: 1000 })
        );
        assert_eq!(check_peer(1000, None), Err(PeerError::UnknownSelf));
    }

    #[test]
    fn we_can_learn_our_own_uid() {
        assert!(own_uid().is_some());
    }
}
