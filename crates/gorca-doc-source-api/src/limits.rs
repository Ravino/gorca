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
    fn clipping_keeps_char_boundaries() {
        let mut s = "aéb".to_owned();
        assert!(clip_text(&mut s, 2));
        assert_eq!(s, "a");
        let mut t = "abc".to_owned();
        assert!(!clip_text(&mut t, 3));
    }
}
