//! AT-SPI events as the source sees them, and the dirty set that turns an event burst into a
//! small refetch plan (ADR-0003 sections 9.1-9.3, normalisation AS-4).
//!
//! The source never trusts an event as a delta (P-9 section 1.1, point 5): `ChildrenChanged` means
//! "re-list this node's children", a name or state event means "re-read this node". The event
//! payload is used only to find *which* node, so a lost or duplicated event costs one redundant
//! refetch, not a wrong mirror.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bitflags::bitflags;

use crate::wire::PathKey;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EventKind {
    /// `ChildrenChanged:add`. `child` is `None` for index -1 or a null reference (Firefox).
    ChildAdded {
        index: i32,
        child: Option<PathKey>,
    },
    ChildRemoved {
        index: i32,
        child: Option<PathKey>,
    },
    NameChanged,
    DescriptionChanged,
    ValueChanged,
    StateChanged {
        state: Arc<str>,
        on: bool,
    },
    TextChanged {
        insert: bool,
    },
    /// `StateChanged:defunct` or an explicit destroy.
    Defunct,
}

#[derive(Clone, Debug)]
pub struct AtspiEvent {
    /// The object that emitted the event.
    pub path: PathKey,
    pub kind: EventKind,
    pub at: Instant,
}

impl AtspiEvent {
    /// Normalise a raw `org.a11y.atspi.Event.Object` signal. `member` is the signal name,
    /// `detail` its first argument, `d1`/`d2` the two integers, `child` the object reference in
    /// the variant when it is one. The `/system` suffix Firefox appends to the detail is dropped.
    pub fn from_object_signal(
        path: PathKey,
        member: &str,
        detail: &str,
        d1: i32,
        d2: i32,
        child: Option<PathKey>,
        at: Instant,
    ) -> Option<Self> {
        let detail = detail.strip_suffix("/system").unwrap_or(detail);
        let kind = match member {
            "ChildrenChanged" => match detail {
                "add" => EventKind::ChildAdded {
                    index: d1,
                    child: child.filter(|_| d1 >= 0),
                },
                "remove" => EventKind::ChildRemoved {
                    index: d1,
                    child: child.filter(|_| d1 >= 0),
                },
                _ => return None,
            },
            "PropertyChange" => match detail {
                "accessible-name" => EventKind::NameChanged,
                "accessible-description" => EventKind::DescriptionChanged,
                "accessible-value" => EventKind::ValueChanged,
                _ => return None,
            },
            "StateChanged" => {
                if detail == "defunct" && d1 != 0 {
                    EventKind::Defunct
                } else {
                    EventKind::StateChanged {
                        state: Arc::from(detail),
                        on: d1 != 0,
                    }
                }
            }
            "TextChanged" => EventKind::TextChanged {
                insert: detail.starts_with("insert"),
            },
            _ => return None,
        };
        let _ = d2;
        Some(Self { path, kind, at })
    }
}

bitflags! {
    #[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
    pub struct Dirty: u8 {
        /// Re-list the children of this node.
        const CHILDREN = 1;
        /// Re-read role/state/name/attributes of this node.
        const PROPS = 2;
        /// Re-read this node's text.
        const TEXT = 4;
    }
}

/// States whose changes are not the document source's business: focus and caret come from the
/// AT-SPI connector (SL-5).
fn ignorable_state(s: &str) -> bool {
    matches!(
        s,
        "focused" | "active" | "armed" | "animated" | "stale" | "opaque"
    )
}

/// When a burst of events is considered over (ADR-0003 9.3).
#[derive(Clone, Copy, Debug)]
pub struct SettlePolicy {
    /// Silence needed before a burst is considered over.
    pub quiet: Duration,
    /// A burst is processed at most this long after its first event.
    pub max_wait: Duration,
    /// A burst with at least this many `ChildRemoved` events is a move or a re-render in
    /// progress: Firefox implements a DOM move as remove + add, delivered over >100 ms, and a
    /// refresh in the middle sees the items as gone and then as new (their logical ids are
    /// lost). Such a burst may wait up to `storm_max_wait` for the page to go quiet.
    pub storm_removes: u32,
    pub storm_max_wait: Duration,
}

impl Default for SettlePolicy {
    fn default() -> Self {
        Self {
            quiet: Duration::from_millis(25),
            max_wait: Duration::from_millis(120),
            storm_removes: 50,
            storm_max_wait: Duration::from_millis(1000),
        }
    }
}

/// Events accumulated since the last plan.
#[derive(Debug, Default)]
pub struct DirtySet {
    map: HashMap<PathKey, Dirty>,
    first: Option<Instant>,
    last: Option<Instant>,
    events: u32,
    removes: u32,
}

#[derive(Debug)]
pub struct Taken {
    pub map: HashMap<PathKey, Dirty>,
    pub first_event: Option<Instant>,
    pub last_event: Option<Instant>,
    pub events: u32,
}

impl DirtySet {
    pub fn note(&mut self, ev: &AtspiEvent) {
        let bits = match &ev.kind {
            EventKind::ChildAdded { .. } => Dirty::CHILDREN,
            EventKind::ChildRemoved { .. } => {
                self.removes = self.removes.saturating_add(1);
                Dirty::CHILDREN
            }
            EventKind::NameChanged | EventKind::DescriptionChanged | EventKind::ValueChanged => {
                Dirty::PROPS
            }
            EventKind::StateChanged { state, .. } => {
                if ignorable_state(state) {
                    return;
                }
                Dirty::PROPS
            }
            EventKind::TextChanged { .. } => Dirty::TEXT,
            // The parent's own ChildrenChanged:remove covers the structure.
            EventKind::Defunct => return,
        };
        *self.map.entry(ev.path).or_default() |= bits;
        self.first.get_or_insert(ev.at);
        self.last = Some(ev.at);
        self.events = self.events.saturating_add(1);
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn events(&self) -> u32 {
        self.events
    }

    /// Settle rule (ADR-0003 9.3): wait for `quiet` of silence, but never longer than `max_wait`
    /// since the first event of the burst (`storm_max_wait` for a move in progress).
    pub fn deadline(&self, p: &SettlePolicy) -> Option<Instant> {
        let (first, last) = (self.first?, self.last?);
        let max = if self.removes >= p.storm_removes {
            p.storm_max_wait
        } else {
            p.max_wait
        };
        Some((last + p.quiet).min(first + max))
    }

    pub fn take(&mut self) -> Taken {
        self.removes = 0;
        Taken {
            map: std::mem::take(&mut self.map),
            first_event: self.first.take(),
            last_event: self.last.take(),
            events: std::mem::take(&mut self.events),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(kind: EventKind) -> AtspiEvent {
        AtspiEvent {
            path: PathKey(5),
            kind,
            at: Instant::now(),
        }
    }

    #[test]
    fn system_suffix_and_null_children_are_normalised() {
        let now = Instant::now();
        let e = AtspiEvent::from_object_signal(
            PathKey(1),
            "ChildrenChanged",
            "add/system",
            -1,
            0,
            Some(PathKey(9)),
            now,
        )
        .unwrap();
        assert_eq!(
            e.kind,
            EventKind::ChildAdded {
                index: -1,
                child: None
            }
        );
        let e = AtspiEvent::from_object_signal(
            PathKey(1),
            "ChildrenChanged",
            "remove",
            3,
            0,
            Some(PathKey(9)),
            now,
        )
        .unwrap();
        assert_eq!(
            e.kind,
            EventKind::ChildRemoved {
                index: 3,
                child: Some(PathKey(9))
            }
        );
        assert!(
            AtspiEvent::from_object_signal(PathKey(1), "BoundsChanged", "", 0, 0, None, now)
                .is_none()
        );
    }

    #[test]
    fn dirty_set_merges_and_ignores_focus() {
        let mut d = DirtySet::default();
        d.note(&ev(EventKind::ChildAdded {
            index: 0,
            child: None,
        }));
        d.note(&ev(EventKind::NameChanged));
        d.note(&ev(EventKind::StateChanged {
            state: "focused".into(),
            on: true,
        }));
        assert_eq!(d.len(), 1);
        assert_eq!(d.events(), 2);
        let t = d.take();
        assert_eq!(t.map[&PathKey(5)], Dirty::CHILDREN | Dirty::PROPS);
        assert!(d.is_empty());
    }

    #[test]
    fn settle_deadline_is_capped_by_max_wait() {
        let mut d = DirtySet::default();
        let t0 = Instant::now();
        let mut e = ev(EventKind::NameChanged);
        e.at = t0;
        d.note(&e);
        e.at = t0 + Duration::from_millis(90);
        d.note(&e);
        let p = SettlePolicy {
            quiet: Duration::from_millis(30),
            max_wait: Duration::from_millis(100),
            ..SettlePolicy::default()
        };
        let dl = d.deadline(&p).unwrap();
        assert_eq!(dl, t0 + Duration::from_millis(100));
    }

    #[test]
    fn a_move_in_progress_may_wait_longer() {
        let p = SettlePolicy {
            quiet: Duration::from_millis(30),
            max_wait: Duration::from_millis(100),
            storm_removes: 3,
            storm_max_wait: Duration::from_millis(900),
        };
        let mut d = DirtySet::default();
        let t0 = Instant::now();
        for i in 0..4u64 {
            let mut e = ev(EventKind::ChildRemoved {
                index: 0,
                child: None,
            });
            e.at = t0 + Duration::from_millis(i * 20);
            d.note(&e);
        }
        // Still arriving: the quiet rule decides, not the 100 ms cap.
        assert_eq!(d.deadline(&p), Some(t0 + Duration::from_millis(60 + 30)));
        let _ = d.take();
        let mut e = ev(EventKind::NameChanged);
        e.at = t0;
        d.note(&e);
        e.at = t0 + Duration::from_millis(90);
        d.note(&e);
        assert_eq!(
            d.deadline(&p),
            Some(t0 + Duration::from_millis(100)),
            "no removes: plain cap"
        );
    }
}
