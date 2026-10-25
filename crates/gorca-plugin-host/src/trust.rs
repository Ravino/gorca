// SPDX-License-Identifier: GPL-3.0-or-later
//! Trust tiers, permissions and system scopes (ADR-0026 §2.7).
//!
//! Everything here is deny-by-default: a plugin holds no permission until the manifest declares
//! it, the tier allows it and (where required) the user consented.

use bitflags::bitflags;

/// Trust tier of a loaded package (ADR-0026 §2.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TrustTier {
    /// T2: built from this repository, pinned by sha256.
    FirstParty,
    /// T3 the user explicitly marked trusted.
    TrustedThirdParty,
    /// T3 sideloaded, off until enabled.
    Untrusted,
    /// T3-dev: `--ext-dev DIR`, always T3 rules, trust lasts for the session.
    Dev,
}

impl TrustTier {
    /// True for every tier except first-party.
    #[must_use]
    pub fn is_third_party(self) -> bool {
        match self {
            Self::FirstParty => false,
            Self::TrustedThirdParty | Self::Untrusted | Self::Dev => true,
        }
    }
}

bitflags! {
    /// Permissions of the 0.10 table. Nothing is held by default.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct Perms: u16 {
        /// Names and descriptions in reads.
        const READ_NAMES = 1 << 0;
        /// Own text, `text`, text requests.
        const READ_TEXT = 1 << 1;
        /// `attr`.
        const READ_ATTRIBUTES = 1 << 2;
        /// `facts.*`.
        const CONTRIBUTE_FACTS = 1 << 3;
        /// `output.*`.
        const SPEAK = 1 << 4;
        /// `move-cursor`, `request-mode`.
        const NAVIGATE = 1 << 5;
        /// `activate`, `focus`.
        const ACT = 1 << 6;
        /// `pointer-click`.
        const POINTER = 1 << 7;
        /// State directory quota.
        const STORAGE = 1 << 8;
    }
}

/// A permission name that does not exist in 0.10.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PermError {
    /// `net:*`, `clipboard-*`, `observe-typing`, `observe-all-apps` are not in 0.10.
    #[error("permission `{0}` is not available in 0.10")]
    NotInThisVersion(String),
    /// Not a permission name.
    #[error("unknown permission `{0}`")]
    Unknown(String),
    /// The tier may never hold it.
    #[error("permission `{0}` is never granted to this tier")]
    ForbiddenForTier(&'static str),
}

/// Parse a manifest permission name.
pub fn parse_perm(name: &str) -> Result<Perms, PermError> {
    match name {
        "read-names" => Ok(Perms::READ_NAMES),
        "read-text" => Ok(Perms::READ_TEXT),
        "read-attributes" => Ok(Perms::READ_ATTRIBUTES),
        "contribute-facts" => Ok(Perms::CONTRIBUTE_FACTS),
        "speak" => Ok(Perms::SPEAK),
        "navigate" => Ok(Perms::NAVIGATE),
        "act" => Ok(Perms::ACT),
        "pointer" => Ok(Perms::POINTER),
        "storage" => Ok(Perms::STORAGE),
        n if n.starts_with("net:")
            || n.starts_with("clipboard-")
            || n == "observe-typing"
            || n == "observe-all-apps" =>
        {
            Err(PermError::NotInThisVersion(n.to_string()))
        }
        n => Err(PermError::Unknown(n.to_string())),
    }
}

/// Permissions a tier may ever hold, before consent. `pointer` is T2 only (QP-5).
#[must_use]
pub fn tier_ceiling(tier: TrustTier) -> Perms {
    match tier {
        TrustTier::FirstParty => Perms::all(),
        TrustTier::TrustedThirdParty | TrustTier::Untrusted | TrustTier::Dev => {
            Perms::all().difference(Perms::POINTER)
        }
    }
}

/// Permissions that need per-plugin user consent for third parties (AC-3 `act`).
const CONSENT_FOR_THIRD_PARTY: Perms = Perms::ACT;

/// The effective grant: `declared ∩ ceiling(tier)`, minus consent-gated bits the user has not
/// confirmed. `declared` empty means nothing is granted.
#[must_use]
pub fn effective_grants(declared: Perms, tier: TrustTier, consented: Perms) -> Perms {
    let mut g = declared & tier_ceiling(tier);
    if tier.is_third_party() {
        g.remove(CONSENT_FOR_THIRD_PARTY.difference(consented));
    }
    g
}

/// The system scopes SS-1..SS-4 (ADR-0026 §2.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SystemScope {
    /// SS-1 authentication prompts, unlock and login screens.
    Authentication,
    /// SS-2 Shell modal system dialogs.
    ShellDialog,
    /// SS-3 secret nodes and their subtrees.
    SecretNode,
    /// SS-4 gorca's own UI.
    GorcaUi,
}

impl SystemScope {
    /// Every system scope.
    pub const ALL: [Self; 4] = [
        Self::Authentication,
        Self::ShellDialog,
        Self::SecretNode,
        Self::GorcaUi,
    ];
}

/// May a plugin of `tier` match, read, target, bind keys in or speak over `scope`?
/// Third parties (every non-first-party tier) never may.
#[must_use]
pub fn tier_may_enter(tier: TrustTier, scope: SystemScope) -> bool {
    match (tier, scope) {
        (
            TrustTier::FirstParty,
            SystemScope::Authentication
            | SystemScope::ShellDialog
            | SystemScope::SecretNode
            | SystemScope::GorcaUi,
        ) => true,
        (
            TrustTier::TrustedThirdParty | TrustTier::Untrusted | TrustTier::Dev,
            SystemScope::Authentication
            | SystemScope::ShellDialog
            | SystemScope::SecretNode
            | SystemScope::GorcaUi,
        ) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const THIRD: [TrustTier; 3] = [
        TrustTier::TrustedThirdParty,
        TrustTier::Untrusted,
        TrustTier::Dev,
    ];

    #[test]
    fn nothing_declared_nothing_granted() {
        for t in [TrustTier::FirstParty].into_iter().chain(THIRD) {
            assert_eq!(
                effective_grants(Perms::empty(), t, Perms::all()),
                Perms::empty()
            );
        }
    }

    #[test]
    fn third_party_never_holds_pointer_and_needs_consent_for_act() {
        for t in THIRD {
            let g = effective_grants(
                Perms::POINTER | Perms::ACT | Perms::SPEAK,
                t,
                Perms::empty(),
            );
            assert_eq!(g, Perms::SPEAK);
            let g = effective_grants(Perms::POINTER | Perms::ACT, t, Perms::ACT | Perms::POINTER);
            assert_eq!(g, Perms::ACT);
        }
        let g = effective_grants(
            Perms::POINTER | Perms::ACT,
            TrustTier::FirstParty,
            Perms::empty(),
        );
        assert_eq!(g, Perms::POINTER | Perms::ACT);
    }

    #[test]
    fn third_parties_never_enter_a_system_scope() {
        for t in THIRD {
            for s in SystemScope::ALL {
                assert!(!tier_may_enter(t, s), "{t:?} {s:?}");
            }
        }
        for s in SystemScope::ALL {
            assert!(tier_may_enter(TrustTier::FirstParty, s));
        }
    }

    #[test]
    fn permission_names() {
        assert_eq!(parse_perm("speak"), Ok(Perms::SPEAK));
        assert!(matches!(
            parse_perm("net:any"),
            Err(PermError::NotInThisVersion(_))
        ));
        assert!(matches!(
            parse_perm("clipboard-read"),
            Err(PermError::NotInThisVersion(_))
        ));
        assert!(matches!(parse_perm("root"), Err(PermError::Unknown(_))));
    }
}
