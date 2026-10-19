// SPDX-License-Identifier: GPL-3.0-or-later
//! Pure decoding of AT-SPI replies into core values (C-4, C-6). Nothing here touches the bus, so
//! everything is unit-tested with hostile inputs. All of it is untrusted data.

use std::sync::Arc;
use std::time::Instant;

use gorca_model::sem::SrcRecord;
use gorca_model::{
    AttrKey, AttrMap, ChildList, Freshness, Ifaces, NodeKey, NodeSem, NodeSnapshot, PropMask,
    Relations, Role, StateSet, Value,
};
use gorca_sem_atspi::StateBits;
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

/// Connector-normalised states: the shared flat mapping in its native-application form
/// (`DISABLED` follows Orca: a node without `Sensitive` is unavailable, but only when the
/// application reported a state set at all).
pub fn states_from_raw(words: &[u32]) -> StateSet {
    gorca_sem_atspi::states::state_set(
        StateBits(state_bits(words)),
        gorca_sem_atspi::states::StateMode::Native,
    )
}

/// Everything a focus presentation needs, as the connector learned it.
#[derive(Clone, Debug, Default)]
pub struct RawNode {
    pub role: u32,
    pub state_words: Vec<u32>,
    pub name: Option<String>,
    pub description: Option<String>,
    /// The `aria-errormessage` text of an invalid web control (read on a focus fetch).
    pub error_text: Option<String>,
    pub value: Option<RawValue>,
    /// Parent reference, already resolved to a key of this app (foreign refs are dropped).
    pub parent: Option<NodeKey>,
    /// Child count as the app reported it, clipped to `[0, MAX_CHILD_COUNT]`.
    pub child_count: Option<u32>,
    /// Child keys when the whole list is known (a dump, a children walk).
    pub children: Option<SmallVec<[NodeKey; 8]>>,
    pub interfaces: Ifaces,
    /// The object attributes the core understands (`GetAttributes`, see [`attr_key`]), in wire
    /// order, values clipped to [`MAX_ATTR_VALUE_BYTES`] and at most [`MAX_ATTRS`] of them. Only
    /// filled when `have` carries `ATTRS`.
    pub attrs: Vec<(AttrKey, String)>,
    /// The attribute pairs the normaliser reads (`gorca-sem-atspi`), raw names, values clipped
    /// to [`MAX_ATTR_VALUE_BYTES`], at most [`MAX_ATTRS`] (first occurrence of a name wins).
    /// Filled together with `attrs`; see [`keep_sem_attrs`].
    pub sem_attrs: Vec<(Box<str>, Box<str>)>,
    /// Fields this node actually carries.
    pub have: PropMask,
}

/// Cap on the attributes kept per node: the app's attribute map is untrusted and can be large
/// (Chromium exports long `class` strings, which are not kept at all).
pub const MAX_ATTRS: usize = 32;
/// Cap on one attribute value.
pub const MAX_ATTR_VALUE_BYTES: usize = 256;

/// The core key of a toolkit object attribute, `None` for the ones nothing reads (`class`, `id`,
/// styling, ...). Keys are the well-known ones of `web_attrs`, so a web node fetched here carries
/// the same attributes the document source delivers.
pub fn attr_key(name: &str) -> Option<AttrKey> {
    use gorca_doc_source_api::web_attrs as w;
    Some(match name {
        "tag" => w::TAG,
        "xml-roles" => w::XML_ROLES,
        "level" => w::LEVEL,
        "live" => w::LIVE,
        "atomic" => w::ATOMIC,
        "relevant" => w::RELEVANT,
        "busy" => w::BUSY,
        "posinset" => w::POSINSET,
        "setsize" => w::SETSIZE,
        "rowindex" => w::ROWINDEX,
        "colindex" => w::COLINDEX,
        "rowcount" => w::ROWCOUNT,
        "colcount" => w::COLCOUNT,
        "container-live" => w::CONTAINER_LIVE,
        "display" => w::DISPLAY,
        "layout-guess" => w::LAYOUT_GUESS,
        "current" => w::CURRENT,
        "haspopup" => w::HASPOPUP,
        "explicit-name" => w::EXPLICIT_NAME,
        _ => return None,
    })
}

/// The kept attributes of one `GetAttributes` reply: unknown names dropped, values clipped,
/// the first occurrence of a key wins, at most [`MAX_ATTRS`], sorted by key.
pub fn keep_attrs<'a>(
    pairs: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Vec<(AttrKey, String)> {
    let mut out: Vec<(AttrKey, String)> = Vec::new();
    for (k, v) in pairs {
        if out.len() >= MAX_ATTRS {
            break;
        }
        if let Some(key) = attr_key(k)
            && !out.iter().any(|(o, _)| *o == key)
        {
            out.push((key, clip(v, MAX_ATTR_VALUE_BYTES).to_owned()));
        }
    }
    // The wire order of a dictionary is the sender's (hash) order: sort, so that two fetches of
    // one node compare equal.
    out.sort_by_key(|(k, _)| *k);
    out
}

/// The pairs of one `GetAttributes` reply that the normaliser reads (`gorca-sem-atspi`
/// `WANTED_ATTRS`): unknown names dropped (`class`, `id`, styling are not copied at all), values
/// clipped, the first occurrence of a name wins, at most [`MAX_ATTRS`], in wire order.
pub fn keep_sem_attrs<'a>(
    pairs: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Vec<(Box<str>, Box<str>)> {
    let mut out: Vec<(Box<str>, Box<str>)> = Vec::new();
    for (k, v) in pairs {
        if out.len() >= MAX_ATTRS {
            break;
        }
        if gorca_sem_atspi::attrs::wants_attr(k) && !out.iter().any(|(o, _)| &**o == k) {
            out.push((Box::from(k), Box::from(clip(v, MAX_ATTR_VALUE_BYTES))));
        }
    }
    out
}

/// What the normaliser makes of one raw node (the dual-carried source record, M22a-C1b). Pure.
pub fn source_record(raw: &RawNode) -> SrcRecord {
    let pairs: SmallVec<[(&str, &str); 16]> =
        raw.sem_attrs.iter().map(|(k, v)| (&**k, &**v)).collect();
    let lo = raw.state_words.first().copied().unwrap_or(0);
    let hi = raw.state_words.get(1).copied().unwrap_or(0);
    gorca_sem_atspi::normalise(&gorca_sem_atspi::RawNode {
        fetched: raw.have
            & (PropMask::ROLE
                | PropMask::STATES
                | PropMask::ATTRS
                | PropMask::NAME
                | PropMask::DESCRIPTION),
        role: raw.role,
        states: StateBits::from_words(lo, hi),
        attrs: &pairs,
        name: raw.name.as_deref(),
        description: raw.description.as_deref(),
    })
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
        attrs: {
            let mut a: AttrMap = raw
                .attrs
                .iter()
                .map(|(k, v)| (*k, Arc::from(v.as_str())))
                .collect();
            if let Some(t) = non_empty(raw.error_text.as_deref()) {
                a.push((gorca_doc_source_api::web_attrs::ERROR_TEXT, t));
            }
            a
        },
        relations: Relations::default(),
        bounds: None,
        have: raw.have,
        confirmed_seq: 0,
        fresh: Freshness::Fresh { at: now },
        sem: NodeSem {
            rec: Some(Arc::new(source_record(raw))),
            entry: None,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use atspi::State as Raw;
    use gorca_model::{AppId, ConnectorId, LocalId};

    #[test]
    fn indeterminate_decodes_to_mixed_and_expandable_to_one_expansion_state() {
        let mixed = states_from_raw(&words(&[
            Raw::Enabled,
            Raw::Sensitive,
            Raw::Checkable,
            Raw::Indeterminate,
        ]));
        assert!(mixed.contains(StateSet::MIXED));
        let col = states_from_raw(&words(&[Raw::Enabled, Raw::Sensitive, Raw::Expandable]));
        assert!(col.contains(StateSet::COLLAPSED) && !col.contains(StateSet::EXPANDED));
        let exp = states_from_raw(&words(&[
            Raw::Enabled,
            Raw::Sensitive,
            Raw::Expandable,
            Raw::Expanded,
        ]));
        assert!(exp.contains(StateSet::EXPANDED) && !exp.contains(StateSet::COLLAPSED));
    }

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
    fn attrs_keep_known_keys_clip_values_and_drop_class() {
        use gorca_doc_source_api::web_attrs as w;
        let big = "c".repeat(100_000);
        let kept = keep_attrs([
            ("class", big.as_str()),
            ("id", "x"),
            ("tag", "button"),
            ("xml-roles", "switch"),
            ("tag", "div"),
            ("explicit-name", big.as_str()),
        ]);
        assert_eq!(kept.len(), 3);
        assert_eq!(kept[0], (w::TAG, "button".to_owned()));
        assert_eq!(kept[2].1.len(), MAX_ATTR_VALUE_BYTES);
        let many: Vec<(String, String)> = (0..500)
            .map(|i| (format!("k{i}"), "v".to_owned()))
            .collect();
        assert!(keep_attrs(many.iter().map(|(k, v)| (k.as_str(), v.as_str()))).is_empty());
        let raw = RawNode {
            role: atspi::Role::Button as u32,
            attrs: kept,
            error_text: Some("bad".to_owned()),
            have: PropMask::ATTRS,
            ..RawNode::default()
        };
        let n = snapshot(key(), &raw, Instant::now());
        assert_eq!(n.attrs.len(), 4);
        assert!(n.attrs.iter().any(|(k, _)| *k == w::ERROR_TEXT));
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
