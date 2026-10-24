// SPDX-License-Identifier: GPL-3.0-or-later
//! Static tables `FUSE` reads: the observed trust table `PI`, the `inexpressible` row, the role
//! applicability rules behind repair `R` and the S3 rules, and the default role. All of it is
//! data per source profile (ADR-0024 section 2.2, ADR-0016), never code.

use gorca_model::sem::{GROUP_COUNT, Group, GroupMask, SemRole};

use crate::register::{MAX_PRIMARIES, PrimarySlot};

/// A set of roles as a bit set over the `u16` role codes (256 bits; codes at or above 256 are
/// never members, and a build-time assert fails if the registry outgrows the set).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct RoleSet([u64; 4]);

const _: () = assert!(SemRole::ALL.len() <= 256);

impl RoleSet {
    pub const EMPTY: RoleSet = RoleSet([0; 4]);

    pub fn of(roles: &[SemRole]) -> RoleSet {
        roles.iter().fold(RoleSet::EMPTY, |s, r| s.with(*r))
    }

    /// The set of every role `pred` holds for.
    pub fn of_pred(pred: fn(SemRole) -> bool) -> RoleSet {
        SemRole::ALL
            .iter()
            .filter(|r| pred(**r))
            .fold(RoleSet::EMPTY, |s, r| s.with(*r))
    }

    #[must_use]
    pub const fn with(mut self, r: SemRole) -> RoleSet {
        let c = r.code();
        if c < 256 {
            self.0[(c >> 6) as usize] |= 1u64 << (c & 63);
        }
        self
    }

    pub const fn has(self, r: SemRole) -> bool {
        let c = r.code();
        c < 256 && self.0[(c >> 6) as usize] & (1u64 << (c & 63)) != 0
    }
}

/// Role applicability, per profile: which roles carry which role-dependent group (repair `R`,
/// SM-1/SM-2/SM-5) and which roles derive a name from content or from a title attribute (S3).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Rules {
    /// Roles whose `check` is meaningful; elsewhere `check` is `NotCheckable`.
    pub checkable: RoleSet,
    /// Roles whose `avail` is meaningful; elsewhere `NotApplicable` (SM-5).
    pub disableable: RoleSet,
    /// Roles whose `expand` is meaningful.
    pub expandable: RoleSet,
    /// Roles that carry a `level`.
    pub level_roles: RoleSet,
    /// Roles named from their content (S3 name from content).
    pub content_named: RoleSet,
    /// How many levels of nameless containers S3 name from content looks through (accname 2F:
    /// a child without a name of its own contributes its own content). 0 reads the final names
    /// of the direct children only (the oracle corpus's rule).
    pub content_depth: u8,
    /// Container roles whose engine-computed name (`namefrom` = content) is replaced by the name
    /// from content when that adds text: Blink names a row from the few children it sees (a
    /// message row from its one button), where accname 2F reads the whole subtree. An authored
    /// name (`aria-label`, `aria-labelledby`) is never replaced.
    pub content_over_engine: RoleSet,
    /// Roles that a `title` evidence fact may name (S3).
    pub title_named: RoleSet,
}

impl Rules {
    /// ARIA 1.2 applicability (supported-states tables) for the registered roles.
    pub fn aria() -> Rules {
        use SemRole as R;
        Rules {
            checkable: RoleSet::of(&[
                R::Checkbox,
                R::Radio,
                R::Switch,
                R::MenuItemCheckbox,
                R::MenuItemRadio,
                // Checkable only when the engine says so (`check_optional` in the normaliser).
                R::Option,
                R::TreeItem,
                R::MenuItem,
                // Indeterminate (no value) is the mixed state of a progress bar.
                R::ProgressBar,
                R::Meter,
            ]),
            disableable: RoleSet::of_pred(SemRole::is_disableable),
            expandable: RoleSet::of(&[
                R::Application,
                R::Button,
                R::Checkbox,
                R::Combobox,
                R::GridCell,
                R::Link,
                R::Listbox,
                R::MenuItem,
                R::Row,
                R::RowHeader,
                R::Tab,
                R::TreeItem,
            ]),
            level_roles: RoleSet::of(&[R::Heading, R::ListItem, R::Row, R::TreeItem]),
            content_depth: 8,
            content_over_engine: RoleSet::of(&[R::Cell, R::GridCell, R::ListItem, R::Row]),
            content_named: RoleSet::of(&[
                R::Article,
                R::Button,
                R::Cell,
                R::Checkbox,
                R::ColumnHeader,
                R::GridCell,
                R::Heading,
                R::Link,
                R::ListItem,
                R::MenuItem,
                R::MenuItemCheckbox,
                R::MenuItemRadio,
                R::Option,
                R::Radio,
                R::Row,
                R::RowHeader,
                R::Switch,
                R::Tab,
                R::Tooltip,
                R::TreeItem,
            ]),
            title_named: RoleSet::of(&[
                R::Button,
                R::Link,
                R::Img,
                R::Checkbox,
                R::Radio,
                R::Switch,
            ]),
        }
    }
}

/// One group's `PI` row: the primary sources in priority order (at most `MAX_PRIMARIES`).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct PiRow {
    len: u8,
    order: [PrimarySlot; MAX_PRIMARIES],
}

impl PiRow {
    pub fn new(order: &[PrimarySlot]) -> PiRow {
        let mut row = PiRow {
            len: 0,
            order: [PrimarySlot::P0; MAX_PRIMARIES],
        };
        for (dst, src) in row.order.iter_mut().zip(order) {
            *dst = *src;
            row.len = row.len.saturating_add(1);
        }
        row
    }

    pub fn as_slice(&self) -> &[PrimarySlot] {
        self.order.get(..usize::from(self.len)).unwrap_or(&[])
    }
}

/// Resource bounds of `FUSE` (security review of B1a): no input can make a fused string or a
/// derivation walk grow with the size of the tree.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Limits {
    /// Maximum bytes of a `name`/`description` register value and of a derived name (ADR
    /// string limit 1 KiB; truncated at a char boundary). Tests may lower it.
    pub max_text: usize,
    /// Maximum children read by one name-from-content derivation. Depth needs no separate
    /// bound: every child name is already at most `max_text`, so a parent's is too.
    pub max_children: usize,
    /// Maximum nodes in the priority set (focus chain, visible window and the descendants that
    /// feed them): computing it costs at most this much, whatever the tree looks like.
    pub max_priority: usize,
    /// Maximum node fusions of the synchronous priority drain (masking, revocation, global
    /// epoch). What does not fit stays dirty in the front class for the next `run`: the part
    /// that runs inside the caller's turn is bounded (FS-7, M-16: ~0.5 ms).
    pub max_sync_drain: usize,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            max_text: crate::value::MAX_TEXT_BYTES,
            max_children: 256,
            max_priority: 1024,
            max_sync_drain: 64,
        }
    }
}

/// The per-source-profile static tables of one app (ADR-0024 section 2.13 selects the profile).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Profile {
    pi: [PiRow; GROUP_COUNT],
    /// Groups the profile's primaries can never express: only there may a Secondary be a
    /// candidate in S0 (H-3). A dynamic `Unknown` never admits a Secondary.
    pub inexpressible: GroupMask,
    pub rules: Rules,
    pub limits: Limits,
    /// What an unknown or absent `role` finalises to.
    pub default_role: SemRole,
}

impl Profile {
    /// A profile whose every group uses `order` as the primary priority.
    pub fn new(order: &[PrimarySlot], rules: Rules) -> Profile {
        Profile {
            pi: [PiRow::new(order); GROUP_COUNT],
            inexpressible: GroupMask::EMPTY,
            rules,
            limits: Limits::default(),
            default_role: SemRole::default(),
        }
    }

    /// Override the priority of one group (data, per app profile).
    pub fn set_pi(&mut self, g: Group, order: &[PrimarySlot]) {
        if let Some(row) = self.pi.get_mut(usize::from(g.slot())) {
            *row = PiRow::new(order);
        }
    }

    /// The primaries of `g` in priority order.
    pub fn pi(&self, g: Group) -> &[PrimarySlot] {
        self.pi
            .get(usize::from(g.slot()))
            .map_or(&[][..], PiRow::as_slice)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_set_covers_every_registered_role_and_the_high_words() {
        let mut all = RoleSet::EMPTY;
        for r in SemRole::ALL {
            all = all.with(*r);
        }
        assert!(SemRole::ALL.iter().all(|r| all.has(*r)));
        // Codes past the old 128-bit set work (DPUB roles will land there).
        let high = RoleSet::of(&[SemRole::Presentation]);
        assert!(high.has(SemRole::Presentation) && !high.has(SemRole::Button));
    }

    #[test]
    fn role_set_membership() {
        let s = RoleSet::of(&[SemRole::Button, SemRole::Heading]);
        assert!(s.has(SemRole::Button) && s.has(SemRole::Heading));
        assert!(!s.has(SemRole::Link));
        assert!(!RoleSet::EMPTY.has(SemRole::Unknown));
    }

    #[test]
    fn pi_rows_are_per_group_data() {
        let mut p = Profile::new(&[PrimarySlot::P0, PrimarySlot::P1], Rules::aria());
        p.set_pi(Group::Level, &[PrimarySlot::P1, PrimarySlot::P0]);
        assert_eq!(p.pi(Group::Name), &[PrimarySlot::P0, PrimarySlot::P1]);
        assert_eq!(p.pi(Group::Level), &[PrimarySlot::P1, PrimarySlot::P0]);
        // Extra entries beyond MAX_PRIMARIES are dropped, never a panic.
        let row = PiRow::new(&[PrimarySlot::P0, PrimarySlot::P1, PrimarySlot::P0]);
        assert_eq!(row.as_slice().len(), MAX_PRIMARIES);
    }

    #[test]
    fn aria_rules_are_consistent() {
        let r = Rules::aria();
        assert!(r.checkable.has(SemRole::Checkbox) && !r.checkable.has(SemRole::Button));
        assert!(r.disableable.has(SemRole::Button) && !r.disableable.has(SemRole::Paragraph));
        assert!(r.level_roles.has(SemRole::Heading));
    }
}
