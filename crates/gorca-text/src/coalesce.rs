// SPDX-License-Identifier: GPL-3.0-or-later
//! Coalescing of text edits (ADR-0007 section 9, EV-1, EV-5). Consecutive edits that touch each
//! other fold into one net splice, so a burst of typing, a held BackSpace or a log view
//! appending produces one thing to present per batch. Lengths only: the text of the net insert
//! is read from the window afterwards.

/// One edit in scalar coordinates: replace `removed` scalars at `at` by `inserted` scalars.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Splice {
    pub at: u32,
    pub removed: u32,
    pub inserted: u32,
}

impl Splice {
    pub const fn insert(at: u32, len: u32) -> Self {
        Self {
            at,
            removed: 0,
            inserted: len,
        }
    }

    pub const fn delete(at: u32, len: u32) -> Self {
        Self {
            at,
            removed: len,
            inserted: 0,
        }
    }

    pub const fn kind(&self) -> SpliceKind {
        match (self.removed, self.inserted) {
            (0, 0) => SpliceKind::Nop,
            (0, _) => SpliceKind::Insert,
            (_, 0) => SpliceKind::Delete,
            _ => SpliceKind::Replace,
        }
    }

    /// Text length after applying this splice to a text of length `len`.
    pub const fn new_len(&self, len: u32) -> u32 {
        len.saturating_sub(self.removed)
            .saturating_add(self.inserted)
    }

    /// The range of the new text that this splice wrote.
    pub const fn written(&self) -> std::ops::Range<u32> {
        self.at..self.at.saturating_add(self.inserted)
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum SpliceKind {
    Nop,
    Insert,
    Delete,
    Replace,
}

/// The single splice equivalent to applying `a` and then `b` (whose offsets are in the text
/// after `a`), when `b` touches or overlaps what `a` wrote. `None` when the two edits are apart
/// and cannot be one contiguous edit.
pub fn compose(a: Splice, b: Splice) -> Option<Splice> {
    let (p, i) = (u64::from(a.at), u64::from(a.inserted));
    let (q, d) = (u64::from(b.at), u64::from(b.removed));
    // b's removed range [q, q+d) must touch a's written range [p, p+i).
    if q > p + i || q + d < p {
        return None;
    }
    let before = p.saturating_sub(q).min(d); // removed scalars in front of a's insert
    let overlap = (q + d).min(p + i).saturating_sub(q.max(p)); // eaten from a's insert
    let after = d - before - overlap; // removed scalars behind a's insert
    let at = p.min(q);
    let removed = u64::from(a.removed) + before + after;
    let inserted = i - overlap + u64::from(b.inserted);
    Some(Splice {
        at: u32::try_from(at).ok()?,
        removed: u32::try_from(removed).ok()?,
        inserted: u32::try_from(inserted).ok()?,
    })
}

/// Folds a stream of edits of one node into net splices.
#[derive(Clone, Debug, Default)]
pub struct EditCoalescer {
    net: Option<Splice>,
    count: u32,
}

impl EditCoalescer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of edits folded into the pending net splice.
    pub fn count(&self) -> u32 {
        self.count
    }

    pub fn pending(&self) -> Option<Splice> {
        self.net
    }

    /// Add an edit. If it cannot join the pending splice, the pending one is returned (the
    /// caller presents it) and the new edit starts the next.
    pub fn push(&mut self, s: Splice) -> Option<Splice> {
        let Some(cur) = self.net else {
            self.net = Some(s);
            self.count = 1;
            return None;
        };
        if let Some(c) = compose(cur, s) {
            self.net = Some(c);
            self.count += 1;
            None
        } else {
            self.net = Some(s);
            self.count = 1;
            Some(cur)
        }
    }

    /// End of batch: take the pending net splice.
    pub fn take(&mut self) -> Option<Splice> {
        self.count = 0;
        self.net.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_run_is_one_insert() {
        let mut c = EditCoalescer::new();
        for i in 0..5 {
            assert_eq!(c.push(Splice::insert(10 + i, 1)), None);
        }
        assert_eq!(c.count(), 5);
        assert_eq!(c.take(), Some(Splice::insert(10, 5)));
        assert_eq!(c.take(), None);
    }

    #[test]
    fn backspace_run_is_one_delete() {
        let mut c = EditCoalescer::new();
        for i in 0..3 {
            c.push(Splice::delete(9 - i, 1));
        }
        assert_eq!(c.take(), Some(Splice::delete(7, 3)));
    }

    #[test]
    fn delete_key_run_extends_forward() {
        let mut c = EditCoalescer::new();
        for _ in 0..3 {
            c.push(Splice::delete(4, 1));
        }
        assert_eq!(c.take(), Some(Splice::delete(4, 3)));
    }

    #[test]
    fn typing_then_erasing_cancels() {
        let mut c = EditCoalescer::new();
        c.push(Splice::insert(5, 2));
        c.push(Splice::delete(5, 2));
        assert_eq!(c.take().map(|s| s.kind()), Some(SpliceKind::Nop));
    }

    #[test]
    fn apart_edits_flush_the_pending_one() {
        let mut c = EditCoalescer::new();
        assert_eq!(c.push(Splice::insert(0, 1)), None);
        assert_eq!(c.push(Splice::insert(50, 1)), Some(Splice::insert(0, 1)));
        assert_eq!(c.take(), Some(Splice::insert(50, 1)));
    }

    #[test]
    fn replace_kind_and_lengths() {
        let s = compose(Splice::delete(3, 2), Splice::insert(3, 4)).unwrap();
        assert_eq!(s.kind(), SpliceKind::Replace);
        assert_eq!(s.new_len(10), 12);
        assert_eq!(s.written(), 3..7);
    }
}
