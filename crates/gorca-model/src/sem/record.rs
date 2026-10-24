// SPDX-License-Identifier: GPL-3.0-or-later
//! The normalised source record of one node ([`SrcRecord`], ADR-0022 section 2.2, ADR-0024
//! section 2.1, H-7).
//!
//! The record type lives in the model so that a document source can attach it to a `SrcNode`
//! and the document actor can feed it to the fusion without depending on any connector crate;
//! the one function that builds it from raw AT-SPI values is `gorca_sem_atspi::normalise`.

use std::sync::Arc;

use super::{GroupMask, Semantics};

/// Which side decided the role.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum RoleSource {
    /// No usable token; the engine role.
    #[default]
    Engine,
    /// The author's ARIA token.
    Token,
    /// A token was present but the native semantics won (presentational conflict resolution,
    /// secret field, `generic` on a semantic element).
    EngineOverToken,
}

/// Diagnostics of one normalisation, for logs, `explain` and the M-10 coverage counters.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Notes(pub u16);

impl Notes {
    /// The raw role has no model role (`SemRole::Unknown` unless a token decided).
    pub const UNMAPPED_ENGINE_ROLE: Notes = Notes(1 << 0);
    /// `xml-roles` held a token that is not a valid author role (skipped).
    pub const INVALID_ROLE_TOKEN: Notes = Notes(1 << 1);
    /// `xml-roles` held a valid ARIA role the model does not carry (skipped).
    pub const UNMAPPED_ROLE_TOKEN: Notes = Notes(1 << 2);
    /// ROLE was fetched without ATTRS: the role is the engine's, a token could not be consulted.
    pub const TOKEN_UNAVAILABLE: Notes = Notes(1 << 3);
    /// `aria-current` had an unrecognised value (treated as `true`).
    pub const INVALID_CURRENT_TOKEN: Notes = Notes(1 << 4);
    /// `aria-haspopup` had an unrecognised value (treated as `menu`).
    pub const INVALID_POPUP_TOKEN: Notes = Notes(1 << 5);
    /// INVALID_ENTRY on a REQUIRED web field without an `invalid` attribute. The record keeps
    /// `Invalid::True` (a filled `type=email required` holding "abc" is invalid), but engines
    /// also raise the state for an EMPTY required field, which is not an author error:
    /// presentation may suppress `invalid` for this case only when it knows the field is empty.
    pub const INVALID_ENTRY_ON_REQUIRED: Notes = Notes(1 << 6);
    /// A numeric attribute (level, posinset, setsize, index) did not parse.
    pub const MALFORMED_NUMBER: Notes = Notes(1 << 7);
    /// A tag longer than [`TagName::CAP`] bytes was truncated.
    pub const TAG_TRUNCATED: Notes = Notes(1 << 8);
    /// A page string (name, description, placeholder, shortcuts, value text, index texts) was cut
    /// to `PAGE_STRING_CAP` bytes.
    pub const TRUNCATED: Notes = Notes(1 << 9);

    /// True when every bit of `other` is set.
    pub const fn contains(self, other: Notes) -> bool {
        self.0 & other.0 == other.0
    }

    /// True when no note was raised.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Add every bit of `other`.
    pub fn add(&mut self, other: Notes) {
        self.0 |= other.0;
    }
}

/// An HTML tag name stored inline (no allocation). Longer names (custom elements) are truncated
/// at a character boundary; [`Notes::TAG_TRUNCATED`] records it.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct TagName {
    len: u8,
    bytes: [u8; TagName::CAP],
}

impl TagName {
    /// Capacity in bytes.
    pub const CAP: usize = 15;

    /// Build from a tag, truncating at a character boundary. Returns the tag and whether it was
    /// truncated.
    pub fn new(tag: &str) -> (TagName, bool) {
        let mut end = tag.len().min(Self::CAP);
        while !tag.is_char_boundary(end) {
            end -= 1;
        }
        let mut bytes = [0u8; Self::CAP];
        bytes[..end].copy_from_slice(&tag.as_bytes()[..end]);
        let len = u8::try_from(end).unwrap_or(0);
        (TagName { len, bytes }, end < tag.len())
    }

    /// The tag; empty when the node has none.
    pub fn as_str(&self) -> &str {
        let n = usize::from(self.len).min(Self::CAP);
        std::str::from_utf8(&self.bytes[..n]).unwrap_or("")
    }

    /// True when the node had no tag.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// The normalised source record of one node from the AT-SPI source: per-group values with the
/// `known` mask (Unknown vs Absent), plus the pieces S3 and the quirk stratum read.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SrcRecord {
    /// Per-group values. A group with a clear `known` bit holds its default.
    pub sem: Semantics,
    /// The accessible name when known and non-empty.
    pub name: Option<Arc<str>>,
    /// Groups this record has an opinion on (fetched), including Absent ones.
    pub known: GroupMask,
    /// The raw `AtspiRole` the record was built from (kept for quirk rows and logs).
    pub raw_role: u32,
    /// Which side decided the role.
    pub role_source: RoleSource,
    /// The DOM tag, when the engine exports it.
    pub tag: TagName,
    /// The HTML `id` the engine exports, capped. Not a group: the engine stamps it on the fused
    /// entry (`SemText::dom_id`) and uses it to bind DOM-id anchors.
    pub dom_id: Option<Arc<str>>,
    /// Diagnostics.
    pub notes: Notes,
}
