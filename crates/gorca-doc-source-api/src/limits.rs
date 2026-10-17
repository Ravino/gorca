// SPDX-License-Identifier: GPL-3.0-or-later
//! Resource caps for page-fed data. Source data is untrusted (SL-3): the source and the
//! translator apply the same caps, cut what exceeds them, count it and go on. Nothing here
//! panics.

/// Deepest level kept in a mirrored document (the root is level 0).
pub const MAX_DEPTH: u32 = 512;
/// Most nodes one document mirror keeps.
pub const MAX_DOC_NODES: usize = 500_000;
/// Most children kept under one node.
pub const MAX_CHILDREN: usize = 100_000;
/// Most bytes of text kept on one node.
pub const MAX_NODE_TEXT_BYTES: usize = 256 * 1024;
/// Most node ids parked in the translator's pending lists (deferred children and orphans).
pub const MAX_PENDING_IDS: usize = 200_000;

/// Most bytes of an accessible name kept (a page can put a megabyte in `aria-label`).
pub const MAX_NAME_BYTES: usize = 4 * 1024;
/// Most bytes of a description or an error message kept.
pub const MAX_DESCRIPTION_BYTES: usize = 2 * 1024;
/// Most bytes of one object attribute value kept.
pub const MAX_ATTR_VALUE_BYTES: usize = 4 * 1024;
/// Most relations and most targets per relation read from one node. The source follows one hop
/// only (a relation target's text, never its own relations), so chains and cycles of
/// `aria-describedby` / `aria-errormessage` / `aria-labelledby` cannot grow the work.
pub const MAX_RELATIONS: usize = 32;
pub const MAX_RELATION_TARGETS: usize = 8;

/// `s` cut to at most `max` bytes on a char boundary.
pub fn clip_str(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Truncate `s` to at most `max` bytes on a char boundary; true if it was cut.
pub fn clip_text(s: &mut String, max: usize) -> bool {
    if s.len() <= max {
        return false;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_str_keeps_char_boundaries() {
        assert_eq!(clip_str("a\u{e9}b", 2), "a");
        assert_eq!(clip_str("abc", 9), "abc");
    }

    #[test]
    fn clipping_keeps_char_boundaries() {
        let mut s = "aéb".to_owned();
        assert!(clip_text(&mut s, 2));
        assert_eq!(s, "a");
        let mut t = "abc".to_owned();
        assert!(!clip_text(&mut t, 3));
    }
}
