// SPDX-License-Identifier: GPL-3.0-or-later
//! Tri-state enums, flags and small value types of `Semantics` (ADR-0022 section 2.1).
//!
//! Mutually exclusive states are unrepresentable (SM-2). "Unknown" is **not** a variant: it is
//! a clear bit in `SemEntry.known` (ADR-0022 section 2.3), so these enums carry only observed
//! values and the first variant of each is the default/"not applicable" value.

use std::num::NonZeroU32;
use std::sync::Arc;

use bitflags::bitflags;

use super::coded::coded_enum;
use crate::event::Politeness;
use crate::ids::NodeKey;

coded_enum! {
    /// `aria-checked` on checkbox, radio, menuitemcheckbox, switch, option.
    pub enum Check: u8 {
        NotCheckable = 0 => "na",
        Off = 1 => "off",
        On = 2 => "on",
        Mixed = 3 => "mixed",
    }
}

coded_enum! {
    /// `aria-pressed` (toggle buttons).
    pub enum Press: u8 {
        NotToggle = 0 => "na",
        Off = 1 => "off",
        On = 2 => "on",
        Mixed = 3 => "mixed",
    }
}

coded_enum! {
    /// `aria-expanded`.
    pub enum Expand: u8 {
        NotExpandable = 0 => "na",
        Collapsed = 1 => "collapsed",
        Expanded = 2 => "expanded",
    }
}

coded_enum! {
    /// `aria-selected`.
    pub enum Select: u8 {
        NotSelectable = 0 => "na",
        Off = 1 => "off",
        On = 2 => "on",
    }
}

coded_enum! {
    /// `aria-current`.
    pub enum Current: u8 {
        No = 0 => "no",
        True = 1 => "true",
        Page = 2 => "page",
        Step = 3 => "step",
        Location = 4 => "location",
        Date = 5 => "date",
        Time = 6 => "time",
    }
}

coded_enum! {
    /// `aria-invalid`.
    pub enum Invalid: u8 {
        No = 0 => "no",
        True = 1 => "true",
        Grammar = 2 => "grammar",
        Spelling = 3 => "spelling",
    }
}

coded_enum! {
    /// `aria-haspopup`.
    pub enum Popup: u8 {
        No = 0 => "no",
        Menu = 1 => "menu",
        Listbox = 2 => "listbox",
        Tree = 3 => "tree",
        Grid = 4 => "grid",
        Dialog = 5 => "dialog",
    }
}

coded_enum! {
    /// Availability. Static content is `NotApplicable`, never "unavailable" (SM-5).
    pub enum Avail: u8 {
        NotApplicable = 0 => "na",
        Enabled = 1 => "enabled",
        Disabled = 2 => "disabled",
    }
}

coded_enum! {
    /// Where the accessible name came from.
    pub enum NameFrom: u8 {
        None = 0 => "none",
        Content = 1 => "content",
        Author = 2 => "author",
        Placeholder = 3 => "placeholder",
    }
}

coded_enum! {
    /// `aria-orientation`.
    pub enum Orientation: u8 {
        NotApplicable = 0 => "na",
        Horizontal = 1 => "horizontal",
        Vertical = 2 => "vertical",
    }
}

coded_enum! {
    /// `aria-sort`.
    pub enum Sort: u8 {
        None = 0 => "none",
        Ascending = 1 => "ascending",
        Descending = 2 => "descending",
        Other = 3 => "other",
    }
}

coded_enum! {
    /// Where a set position came from (SM-6).
    pub enum PosSrc: u8 {
        Author = 0 => "author",
        Engine = 1 => "engine",
        Computed = 2 => "computed",
    }
}

bitflags! {
    /// Boolean semantic flags (group `flags`).
    #[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
    pub struct SemFlags: u32 {
        const REQUIRED = 1 << 0;
        const READ_ONLY = 1 << 1;
        const BUSY = 1 << 2;
        const MODAL = 1 << 3;
        const MULTISELECTABLE = 1 << 4;
        const MULTILINE = 1 << 5;
        const VISITED = 1 << 6;
        const FOCUSABLE = 1 << 7;
        const EDITABLE = 1 << 8;
        const HAS_DETAILS = 1 << 9;
        const HAS_ERROR_MESSAGE = 1 << 10;
        const ACTIVE_DESCENDANT_HOST = 1 << 11;
        const ATOMIC_PRESENTATION = 1 << 12;
        const LAYOUT_TABLE = 1 << 13;
        const SELECTION_FOLLOWS_FOCUS = 1 << 14;
        /// Node owned by a fact, not by a real accessible (ADR-0024 section 2.3.5). Reserved;
        /// the key space and the flag exist, the machinery is M24.
        const SYNTHETIC = 1 << 15;
    }
}

bitflags! {
    /// `aria-relevant`.
    #[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
    pub struct Relevant: u8 {
        const ADDITIONS = 1 << 0;
        const REMOVALS = 1 << 1;
        const TEXT = 1 << 2;
    }
}

/// Position in a set. `size` is `None` when unknown (`aria-setsize=-1`).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct SetPos {
    pub pos: u32,
    pub size: Option<NonZeroU32>,
    pub src: PosSrc,
}

/// Position in a grid (1-based by convention; spans of at least 1).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct GridPos {
    pub row: u32,
    pub col: u32,
    pub row_span: u16,
    pub col_span: u16,
}

/// Live-region properties. An absent `Live` (the `Option` in `Semantics`) means `off`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Live {
    pub politeness: Politeness,
    pub atomic: bool,
    pub relevant: Relevant,
    pub root: bool,
}

/// Rare strings, shared: one `Arc` per node that has any. Equality compares **contents**
/// (derived `PartialEq` through `Arc<SemText>`), never pointers, so two nodes with equal
/// descriptions held in different allocations are equal.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SemText {
    /// Accessible description (`aria-description`, `aria-describedby` text).
    pub description: Option<Arc<str>>,
    /// `aria-roledescription`, capped at 64 scalars by the normaliser.
    pub role_description: Option<Arc<str>>,
    pub error_text: Option<Arc<str>>,
    pub placeholder: Option<Arc<str>>,
    pub key_shortcuts: Option<Arc<str>>,
    /// `aria-valuetext` / GTK valuetext.
    pub value_text: Option<Arc<str>>,
    /// First `aria-details` target, read on demand.
    pub details_target: Option<NodeKey>,
}

impl SemText {
    /// True when every field is empty (the caller then stores `None` instead of an `Arc`).
    pub fn is_empty(&self) -> bool {
        self.description.is_none()
            && self.role_description.is_none()
            && self.error_text.is_none()
            && self.placeholder.is_none()
            && self.key_shortcuts.is_none()
            && self.value_text.is_none()
            && self.details_target.is_none()
    }
}
