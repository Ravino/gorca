// SPDX-License-Identifier: GPL-3.0-or-later
//! Is a line model still true? (ADR-0006 6.5.) Checked on every step, with no messages.
//!
//! An O(1) revision fast path first; then one lock-free mirror read per distinct node of the
//! model, comparing the snapshot with what the zone was built from. `Miss` is not a change (an
//! eviction); `Gone`, an invalidation of a zone field, or a different hash is.

use gorca_mirror::MirrorRead;
use gorca_model::sem::{Avail, Check, Expand, Invalid, Press, Select, SemFlags};
use gorca_model::{Lookup, NodeKey, NodeSnapshot, PropMask, StructFlags, Value};
use smallvec::SmallVec;

use super::model::LineModel;
use crate::present::entry_of;

/// The fields a zone depends on (ADR-0006 6.5 `ZONE_MASK`).
pub const ZONE_MASK: PropMask = PropMask::NAME
    .union(PropMask::VALUE)
    .union(PropMask::ROLE)
    .union(PropMask::STATES)
    .union(PropMask::BOUNDS)
    .union(PropMask::CHILDREN);

/// The structural bits that change whether a zone exists or can be reached.
const STRUCT_BITS: StructFlags = StructFlags::SHOWING
    .union(StructFlags::VISIBLE)
    .union(StructFlags::DEFUNCT)
    .union(StructFlags::FOCUSABLE)
    .union(StructFlags::MULTILINE);

/// The semantic flags that change what a zone says or how it is classified.
const ZONE_FLAGS: SemFlags = SemFlags::REQUIRED.union(SemFlags::TEXT_LABEL);

/// A small multiply-rotate hasher (FxHash): the check runs for every node of a model on a step.
struct Fx(u64);

impl Fx {
    const K: u64 = 0x517c_c1b7_2722_0a95;

    fn word(&mut self, w: u64) {
        self.0 = (self.0.rotate_left(5) ^ w).wrapping_mul(Self::K);
    }

    fn bytes(&mut self, b: &[u8]) {
        let (chunks, rest) = b.as_chunks::<8>();
        for c in chunks {
            self.word(u64::from_le_bytes(*c));
        }
        let mut tail = 0u64;
        for (i, x) in rest.iter().enumerate() {
            tail |= u64::from(*x) << (8 * i);
        }
        self.word(tail ^ (u64::try_from(b.len()).unwrap_or(0) << 56));
    }
}

/// Hash of what flat review says and does with `s`: the fused entry's role, name and the groups
/// the zone words come from, the structural state, the contents of a field and the extents. A
/// fused entry that changes without changing a word (a provisional one replaced by the real
/// fusion) leaves the hash alone.
pub fn snapshot_hash(s: &NodeSnapshot) -> u64 {
    let e = entry_of(s);
    let sem = &e.sem;
    let mut h = Fx(0);
    h.word(u64::from(e.sem.role.code()));
    // The spoken state groups, as the zone says them: which words, not which raw group values
    // (a selectable node that is not selected says nothing more than one that cannot be).
    let spoken = [
        sem.avail == Avail::Disabled,
        sem.check == Check::Mixed || sem.press == Press::Mixed,
        sem.check == Check::On,
        sem.press == Press::On,
        sem.select == Select::On,
        sem.expand == Expand::Expanded,
        sem.expand == Expand::Collapsed,
        sem.invalid != Invalid::No,
    ];
    h.word(spoken.iter().fold(0u64, |a, b| a << 1 | u64::from(*b)));
    h.word(u64::from((sem.flags & ZONE_FLAGS).bits()));
    let st = s.struct_state();
    h.word(u64::from((st.flags() & STRUCT_BITS).bits()) | u64::from(st.is_known()) << 8);
    if let Some(n) = &e.name {
        h.bytes(n.as_bytes());
    }
    h.word(0xff);
    let text = sem.text.as_deref();
    h.bytes(
        text.and_then(|t| t.value_text.as_deref())
            .unwrap_or_default()
            .as_bytes(),
    );
    // A description decides whether a window or a landmark is a stop.
    h.word(u64::from(
        text.and_then(|t| t.description.as_deref())
            .is_some_and(|d| !d.trim().is_empty()),
    ));
    match &s.value {
        Some(Value::Text(t)) => h.bytes(t.as_bytes()),
        Some(Value::Range { current, .. }) => h.word(current.to_bits()),
        None => h.word(0),
    }
    match s.bounds {
        Some(r) => {
            h.word(u64::from(r.x.cast_unsigned()));
            h.word(u64::from(r.y.cast_unsigned()));
            h.word(u64::from(r.w));
            h.word(u64::from(r.h));
        }
        None => h.word(1),
    }
    h.0
}

/// The verdict of [`validity`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Validity {
    Valid,
    /// Nodes whose zones are no longer true.
    Dirty(SmallVec<[NodeKey; 8]>),
}

/// Cap on the dirty set a verdict lists: more than this is a full rebuild anyway (VD-3).
const DIRTY_LIST_MAX: usize = 64;

/// Check `model` against `mirror` (ADR-0006 6.5). `checked_rev` is the revision the Reader last
/// found valid; equal to the mirror's current one means nothing was written since.
pub fn validity(model: &LineModel, mirror: &dyn MirrorRead, checked_rev: u64) -> (Validity, u64) {
    let rev = mirror.revision();
    if rev == model.built_rev || rev == checked_rev {
        return (Validity::Valid, rev);
    }
    let mut dirty: SmallVec<[NodeKey; 8]> = SmallVec::new();
    let mut count = 0usize;
    for c in &*model.checks {
        let bad = match mirror.view(c.node) {
            Lookup::Gone => true,
            Lookup::Hit(v) => v.stale.intersects(ZONE_MASK) || snapshot_hash(&v.snap) != c.hash,
            // Eviction is not change.
            Lookup::Miss => false,
        };
        if bad {
            count += 1;
            if dirty.len() < DIRTY_LIST_MAX {
                dirty.push(c.node);
            }
        }
    }
    if count == 0 {
        (Validity::Valid, rev)
    } else {
        (Validity::Dirty(dirty), rev)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gorca_model::{
        AppId, ChildList, ConnectorId, Freshness, Ifaces, LocalId, NodeSnapshot, Rect, Role,
        StateSet,
    };
    use std::sync::Arc;
    use std::time::Instant;

    fn snap(n: u64) -> NodeSnapshot {
        NodeSnapshot {
            key: NodeKey {
                app: AppId {
                    connector: ConnectorId(1),
                    serial: 1,
                },
                local: LocalId(n),
            },
            role: Role::Button,
            states: StateSet::SHOWING | StateSet::VISIBLE,
            name: Some(Arc::from("OK")),
            description: None,
            value: None,
            parent: None,
            children: ChildList::Unknown,
            interfaces: Ifaces::empty(),
            attrs: gorca_model::AttrMap::default(),
            relations: gorca_model::Relations::default(),
            bounds: Some(Rect {
                x: 1,
                y: 2,
                w: 3,
                h: 4,
            }),
            have: PropMask::PRESENT | PropMask::BOUNDS,
            confirmed_seq: 0,
            fresh: Freshness::Fresh { at: Instant::now() },
            sem: gorca_model::NodeSem::NONE,
        }
        .with_test_record()
    }

    /// `s` with the source record it would have after the edit.
    fn rec(mut s: NodeSnapshot) -> NodeSnapshot {
        s.sem = gorca_model::NodeSem::NONE;
        s.with_test_record()
    }

    #[test]
    fn the_hash_follows_exactly_the_fields_a_zone_uses() {
        let a = snap(1);
        let mut b = a.clone();
        assert_eq!(snapshot_hash(&a), snapshot_hash(&b));
        b.name = Some(Arc::from("Cancel"));
        assert_ne!(snapshot_hash(&a), snapshot_hash(&rec(b)));
        let mut c = a.clone();
        c.bounds = Some(Rect {
            x: 1,
            y: 3,
            w: 3,
            h: 4,
        });
        assert_ne!(snapshot_hash(&a), snapshot_hash(&c));
        // A spoken state group, and a structural bit.
        let mut d = a.clone();
        d.states |= StateSet::DISABLED;
        assert_ne!(snapshot_hash(&a), snapshot_hash(&rec(d)));
        let mut d = a.clone();
        d.states &= !StateSet::SHOWING;
        assert_ne!(snapshot_hash(&a), snapshot_hash(&d));
        // Fields no zone uses do not make a model dirty.
        let mut e = a.clone();
        e.states |= StateSet::FOCUSED;
        e.confirmed_seq = 9;
        assert_eq!(snapshot_hash(&a), snapshot_hash(&e));
        let mut f = a.clone();
        f.states |= StateSet::ENABLED | StateSet::SELECTABLE;
        assert_eq!(snapshot_hash(&a), snapshot_hash(&rec(f)));
    }

    #[test]
    fn a_description_matters_only_as_far_as_it_is_there() {
        let a = snap(1);
        let mut b = a.clone();
        b.description = Some(Arc::from("tip"));
        assert_ne!(snapshot_hash(&a), snapshot_hash(&rec(b)));
        let mut c = a.clone();
        c.description = Some(Arc::from("  "));
        assert_eq!(snapshot_hash(&a), snapshot_hash(&rec(c)));
    }

    #[test]
    fn a_name_and_a_value_do_not_collide() {
        let mut a = snap(1);
        a.name = Some(Arc::from("ab"));
        a.value = Some(Value::Text(Arc::from("c")));
        let mut b = snap(1);
        b.name = Some(Arc::from("a"));
        b.value = Some(Value::Text(Arc::from("bc")));
        assert_ne!(snapshot_hash(&rec(a)), snapshot_hash(&rec(b)));
    }
}
