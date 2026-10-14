//! Selection diffing (ADR-0007 section 8, SL-3). Selections are sets of scalar ranges; the
//! diff says which text became selected and which became unselected.

use std::ops::Range;

use smallvec::SmallVec;

/// Selected text longer than this is announced as a count, not read (SL-3).
pub const MAX_SPOKEN_SCALARS: u32 = 512;

pub type Ranges = SmallVec<[Range<u32>; 2]>;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SelDelta {
    /// Text selected now that was not before.
    pub added: Ranges,
    /// Text that was selected and no longer is.
    pub removed: Ranges,
}

impl SelDelta {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty()
    }

    pub fn added_len(&self) -> u32 {
        total(&self.added)
    }

    pub fn removed_len(&self) -> u32 {
        total(&self.removed)
    }
}

fn total(r: &[Range<u32>]) -> u32 {
    r.iter().map(|r| r.end - r.start).sum()
}

/// Sort, drop empty and inverted ranges, merge overlapping and touching ones.
pub fn normalize(set: &[Range<u32>]) -> Ranges {
    let mut v: SmallVec<[Range<u32>; 4]> =
        set.iter().filter(|r| r.start < r.end).cloned().collect();
    v.sort_unstable_by_key(|r| (r.start, r.end));
    let mut out = Ranges::new();
    for r in v {
        match out.last_mut() {
            Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
            _ => out.push(r),
        }
    }
    out
}

/// `a` minus `b`; both normalized.
fn subtract(a: &[Range<u32>], b: &[Range<u32>]) -> Ranges {
    let mut out = Ranges::new();
    for r in a {
        let mut cur = r.start;
        for x in b {
            if x.end <= cur {
                continue;
            }
            if x.start >= r.end {
                break;
            }
            if x.start > cur {
                out.push(cur..x.start);
            }
            cur = cur.max(x.end);
        }
        if cur < r.end {
            out.push(cur..r.end);
        }
    }
    out
}

/// What changed between two selection sets. Inputs may be unsorted, overlapping or contain
/// empty ranges (a collapsed selection is an empty set).
pub fn diff(old: &[Range<u32>], new: &[Range<u32>]) -> SelDelta {
    let o = normalize(old);
    let n = normalize(new);
    SelDelta {
        added: subtract(&n, &o),
        removed: subtract(&o, &n),
    }
}

#[cfg(test)]
#[allow(clippy::single_range_in_vec_init)]
mod tests {
    use super::*;

    #[test]
    fn extend_shrink_and_move() {
        let d = diff(&[2..5], &[2..8]);
        assert_eq!(d.added.as_slice(), &[5..8]);
        assert!(d.removed.is_empty());
        let d = diff(&[2..8], &[2..5]);
        assert_eq!(d.removed.as_slice(), &[5..8]);
        let d = diff(&[0..3], &[10..12]);
        assert_eq!(d.added.as_slice(), &[10..12]);
        assert_eq!(d.removed.as_slice(), &[0..3]);
        assert_eq!((d.added_len(), d.removed_len()), (2, 3));
    }

    #[test]
    fn identical_empty_and_collapse() {
        assert!(diff(&[1..4], &[1..4]).is_empty());
        assert!(diff(&[], &[]).is_empty());
        assert!(diff(&[3..3], &[]).is_empty());
        let d = diff(&[1..4], &[]);
        assert_eq!(d.removed.as_slice(), &[1..4]);
    }

    #[test]
    fn unsorted_overlapping_input_is_normalized() {
        let d = diff(&[], &[6..9, 0..4, 3..7, 20..20]);
        assert_eq!(d.added.as_slice(), &[0..9]);
        let d = diff(&[0..9], &[2..3, 5..6]);
        assert_eq!(d.removed.as_slice(), &[0..2, 3..5, 6..9]);
    }
}
