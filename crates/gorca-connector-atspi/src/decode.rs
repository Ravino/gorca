//! Pure decoding of AT-SPI replies into core values (C-4, C-6). Nothing here touches the bus, so
//! everything is unit-tested with hostile inputs. All of it is untrusted data.

use std::sync::Arc;
use std::time::Instant;

use atspi::State as Raw;
use gorca_model::{
    AttrMap, ChildList, Freshness, Ifaces, NodeKey, NodeSnapshot, PropMask, Relations, Role,
    StateSet, Value,
};
use smallvec::SmallVec;

use crate::role_map::map_role;

/// Names and values from an application are cut to this many bytes (a hostile app must not make
/// us copy megabytes into an utterance).
pub const MAX_TEXT_BYTES: usize = 2048;

/// Cut at a char boundary.
pub fn clip(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// `GetRole` result. Unknown or out-of-range numbers are `Role::Unknown`, never an error (C-4).
pub fn role_from_raw(raw: u32) -> Role {
    atspi::Role::try_from(raw).map_or(Role::Unknown, map_role)
}

/// `GetState` result: an array of 32-bit words, least significant first. Missing words are zero;
/// words beyond the second are ignored (bit numbers above 63 do not exist).
pub fn state_bits(words: &[u32]) -> u64 {
    let lo = u64::from(words.first().copied().unwrap_or(0));
    let hi = u64::from(words.get(1).copied().unwrap_or(0));
    lo | (hi << 32)
}

fn has(bits: u64, s: Raw) -> bool {
    bits & (s as u64) != 0
}

/// Connector-normalised states. `DISABLED` follows Orca: a node without `Sensitive` is
/// unavailable, but only when the application reported a state set at all.
pub fn states_from_raw(words: &[u32]) -> StateSet {
    let bits = state_bits(words);
    let mut out = StateSet::empty();
    let table = [
        (Raw::Focused, StateSet::FOCUSED),
        (Raw::Focusable, StateSet::FOCUSABLE),
        (Raw::Selected, StateSet::SELECTED),
        (Raw::Checked, StateSet::CHECKED),
        (Raw::Pressed, StateSet::PRESSED),
        (Raw::Expanded, StateSet::EXPANDED),
        (Raw::Collapsed, StateSet::COLLAPSED),
        (Raw::Editable, StateSet::EDITABLE),
        (Raw::Showing, StateSet::SHOWING),
        (Raw::Visible, StateSet::VISIBLE),
        (Raw::Busy, StateSet::BUSY),
        (Raw::Modal, StateSet::MODAL),
        (Raw::MultiLine, StateSet::MULTILINE),
        (Raw::Required, StateSet::REQUIRED),
        (Raw::InvalidEntry, StateSet::INVALID),
        (Raw::HasPopup, StateSet::HAS_POPUP),
        (Raw::Active, StateSet::ACTIVE),
        (Raw::Defunct, StateSet::DEFUNCT),
        (Raw::Enabled, StateSet::ENABLED),
        (Raw::Selectable, StateSet::SELECTABLE),
        (Raw::Transient, StateSet::TRANSIENT),
        (Raw::ManagesDescendants, StateSet::MANAGES_DESCENDANTS),
    ];
    for (raw, core) in table {
        if has(bits, raw) {
            out |= core;
        }
    }
    if bits != 0 && !has(bits, Raw::Sensitive) {
        out |= StateSet::DISABLED;
    }
    out
}

/// Everything a focus presentation needs, as the connector learned it.
#[derive(Clone, Debug, Default)]
pub struct RawNode {
    pub role: u32,
    pub state_words: Vec<u32>,
    pub name: Option<String>,
    pub description: Option<String>,
    pub value: Option<RawValue>,
    /// Parent reference, already resolved to a key of this app (foreign refs are dropped).
    pub parent: Option<NodeKey>,
    /// Child count as the app reported it, clipped to `[0, MAX_CHILD_COUNT]`.
    pub child_count: Option<u32>,
    /// Child keys when the whole list is known (a dump, a children walk).
    pub children: Option<SmallVec<[NodeKey; 8]>>,
    pub interfaces: Ifaces,
    /// Fields this node actually carries.
    pub have: PropMask,
}

/// The app's child count is untrusted: negative is zero, and nothing above this is believed.
pub const MAX_CHILD_COUNT: u32 = 65_536;

pub fn clip_child_count(n: i64) -> u32 {
    u32::try_from(n.max(0))
        .unwrap_or(u32::MAX)
        .min(MAX_CHILD_COUNT)
}

#[derive(Clone, Debug, PartialEq)]
pub enum RawValue {
    Number { current: f64, min: f64, max: f64 },
    Text(String),
}

fn non_empty(s: Option<&str>) -> Option<Arc<str>> {
    s.map(|s| clip(s, MAX_TEXT_BYTES))
        .filter(|s| !s.is_empty())
        .map(Arc::from)
}

/// Build the immutable snapshot of `key`.
pub fn snapshot(key: NodeKey, raw: &RawNode, now: Instant) -> Arc<NodeSnapshot> {
    let value = raw.value.as_ref().and_then(|v| match v {
        RawValue::Number { current, min, max } => {
            // NaN and infinities from a hostile or buggy app would be spoken as such.
            (current.is_finite() && min.is_finite() && max.is_finite()).then_some(Value::Range {
                current: *current,
                min: *min,
                max: *max,
            })
        }
        RawValue::Text(t) => non_empty(Some(t)).map(Value::Text),
    });
    Arc::new(NodeSnapshot {
        key,
        role: role_from_raw(raw.role),
        states: states_from_raw(&raw.state_words),
        name: non_empty(raw.name.as_deref()),
        description: non_empty(raw.description.as_deref()),
        value,
        parent: raw.parent,
        children: match (&raw.children, raw.child_count) {
            (Some(c), _) => ChildList::Known(c.clone()),
            (None, Some(n)) => ChildList::Count(n),
            (None, None) => ChildList::Unknown,
        },
        interfaces: raw.interfaces,
        attrs: AttrMap::default(),
        relations: Relations::default(),
        bounds: None,
        have: raw.have,
        confirmed_seq: 0,
        fresh: Freshness::Fresh { at: now },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gorca_model::{AppId, ConnectorId, LocalId};

    fn key() -> NodeKey {
        NodeKey {
            app: AppId {
                connector: ConnectorId(1),
                serial: 1,
            },
            local: LocalId(7),
        }
    }

    fn words(states: &[Raw]) -> Vec<u32> {
        let bits: u64 = states.iter().fold(0, |a, s| a | (*s as u64));
        vec![
            u32::try_from(bits & 0xffff_ffff).unwrap(),
            u32::try_from(bits >> 32).unwrap(),
        ]
    }

    #[test]
    fn state_numbers_match_the_atspi_abi() {
        // These bit positions are the wire contract (AtspiStateType).
        assert_eq!(Raw::Active as u64, 1 << 1);
        assert_eq!(Raw::Checked as u64, 1 << 4);
        assert_eq!(Raw::Focused as u64, 1 << 12);
        assert_eq!(Raw::Sensitive as u64, 1 << 24);
        assert_eq!(Raw::InvalidEntry as u64, 1 << 36);
    }

    #[test]
    fn states_map_and_unavailable_follows_sensitive() {
        let w = words(&[Raw::Enabled, Raw::Sensitive, Raw::Checked, Raw::Focused]);
        let s = states_from_raw(&w);
        assert!(s.contains(StateSet::CHECKED | StateSet::FOCUSED));
        assert!(!s.contains(StateSet::DISABLED));

        let w = words(&[Raw::Checked, Raw::Showing]);
        assert!(states_from_raw(&w).contains(StateSet::DISABLED));

        // An empty state set means "the app told us nothing", not "everything is disabled".
        assert_eq!(states_from_raw(&[]), StateSet::empty());
        assert_eq!(states_from_raw(&[0, 0]), StateSet::empty());
    }

    #[test]
    fn hostile_state_arrays_do_not_panic() {
        assert_eq!(state_bits(&[]), 0);
        assert_eq!(state_bits(&[1]), 1);
        assert_eq!(state_bits(&[1, 1, 1, 1, 1]), 1 | (1 << 32));
        let _ = states_from_raw(&[u32::MAX; 9]);
    }

    #[test]
    fn roles_map_and_unknown_numbers_are_unknown() {
        assert_eq!(role_from_raw(atspi::Role::Button as u32), Role::Button);
        assert_eq!(role_from_raw(9999), Role::Unknown);
        assert_eq!(role_from_raw(u32::MAX), Role::Unknown);
    }

    #[test]
    fn clip_respects_char_boundaries() {
        let s = "é".repeat(5000);
        let c = clip(&s, MAX_TEXT_BYTES);
        assert!(c.len() <= MAX_TEXT_BYTES && c.len() >= MAX_TEXT_BYTES - 1);
        assert_eq!(clip("abc", 10), "abc");
    }

    #[test]
    fn snapshot_drops_empty_names_and_non_finite_values() {
        let raw = RawNode {
            role: atspi::Role::Slider as u32,
            state_words: words(&[Raw::Sensitive]),
            name: Some(String::new()),
            description: None,
            value: Some(RawValue::Number {
                current: f64::NAN,
                min: 0.0,
                max: 1.0,
            }),
            ..RawNode::default()
        };
        let n = snapshot(key(), &raw, Instant::now());
        assert_eq!(n.role, Role::Slider);
        assert!(n.name.is_none());
        assert!(n.value.is_none());
        assert!(matches!(n.fresh, Freshness::Fresh { .. }));
    }

    #[test]
    fn snapshot_clips_huge_names() {
        let raw = RawNode {
            role: atspi::Role::Label as u32,
            name: Some("x".repeat(1_000_000)),
            ..RawNode::default()
        };
        let n = snapshot(key(), &raw, Instant::now());
        assert_eq!(n.name.as_deref().map(str::len), Some(MAX_TEXT_BYTES));
    }
}
