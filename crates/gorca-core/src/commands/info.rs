// SPDX-License-Identifier: GPL-3.0-or-later
//! Text of the `info.*` and `debug.*` commands: pure functions over mirror snapshots.
//!
//! "Where am I" v1 (ADR-0005 6.4) uses the ancestry the mirror already holds: application,
//! window, the container chain with positions ("list, 3 of 12"), then the object itself.

use std::fmt::Write as _;
use std::sync::Arc;

use gorca_config::Verbosity;
use gorca_model::{ChildList, Freshness, NodeKey, NodeSnapshot, Role, StateSet, Value};

use super::state::LocalTime;
use crate::directory::MirrorDirectory;

const MAX_DEPTH: usize = 24;

/// The node and its ancestors, leaf first, as far as the mirror knows them.
pub(crate) fn ancestry(dir: &MirrorDirectory, leaf: &Arc<NodeSnapshot>) -> Vec<Arc<NodeSnapshot>> {
    let mut out = vec![Arc::clone(leaf)];
    let mut cur = leaf.parent;
    while let Some(p) = cur {
        if out.len() >= MAX_DEPTH {
            break;
        }
        match dir.lookup(p).hit() {
            Some(n) => {
                cur = n.parent;
                out.push(n);
            }
            None => break,
        }
    }
    out
}

fn named(n: &NodeSnapshot) -> Option<&str> {
    n.name.as_deref().filter(|s| !s.is_empty())
}

/// 1-based position of `child` among the known children of `parent`, and their count.
fn position(parent: &NodeSnapshot, child: NodeKey) -> Option<(usize, usize)> {
    match &parent.children {
        ChildList::Known(list) => list
            .iter()
            .position(|k| *k == child)
            .map(|i| (i + 1, list.len())),
        ChildList::Count(_) | ChildList::Unknown => None,
    }
}

/// Roles whose children are "items" worth a position.
fn is_collection(role: Role) -> bool {
    matches!(
        role,
        Role::List
            | Role::TabList
            | Role::Menu
            | Role::MenuBar
            | Role::Tree
            | Role::Toolbar
            | Role::Table
            | Role::Row
    )
}

/// Roles that are worth naming in the container chain.
fn is_container(role: Role) -> bool {
    is_collection(role)
        || matches!(
            role,
            Role::Panel
                | Role::Document
                | Role::DocumentWeb
                | Role::Landmark
                | Role::Popover
                | Role::StatusBar
        )
}

fn state_words(s: StateSet) -> Vec<&'static str> {
    const W: &[(StateSet, &str)] = &[
        (StateSet::DISABLED, "unavailable"),
        (StateSet::CHECKED, "checked"),
        (StateSet::PRESSED, "pressed"),
        (StateSet::SELECTED, "selected"),
        (StateSet::EXPANDED, "expanded"),
        (StateSet::COLLAPSED, "collapsed"),
        (StateSet::REQUIRED, "required"),
        (StateSet::INVALID, "invalid entry"),
        (StateSet::READ_ONLY, "read only"),
        (StateSet::HAS_POPUP, "has popup"),
    ];
    W.iter()
        .filter(|(f, _)| s.contains(*f))
        .map(|(_, w)| *w)
        .collect()
}

fn value_text(v: &Value) -> Option<String> {
    match v {
        Value::Text(t) if !t.is_empty() => Some(t.to_string()),
        Value::Range { current, .. } => Some(format!("{current}")),
        Value::Text(_) => None,
    }
}

/// Spell a text out: `Save` becomes "S a v e" (the speech service reads single letters).
pub(crate) fn spell(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c == ' ' {
                "space".to_owned()
            } else {
                c.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Text of "where am I". `chain` is leaf first. `repeat`: 1 basic, 2 detailed, 3 spell the name.
pub(crate) fn where_am_i(chain: &[Arc<NodeSnapshot>], repeat: u8, level: Verbosity) -> String {
    let Some(leaf) = chain.first() else {
        return "no focus".to_owned();
    };
    if repeat >= 3 {
        return match named(leaf) {
            Some(n) => spell(n),
            None => format!("{}, no name", leaf.role.spoken()),
        };
    }
    let detailed = repeat == 2 || level == Verbosity::Verbose;
    let mut parts: Vec<String> = Vec::new();
    // Application and window.
    let app = chain.iter().rev().find(|n| n.role == Role::Application);
    if let Some(a) = app.and_then(|a| named(a))
        && level != Verbosity::Brief
    {
        parts.push(a.to_owned());
    }
    let window = chain
        .iter()
        .find(|n| matches!(n.role, Role::Window | Role::Dialog));
    if let Some(w) = window {
        let role = w.role.spoken();
        parts.push(match named(w) {
            Some(n) => format!("{n} {role}"),
            None => role.to_owned(),
        });
    }
    // Container chain between the window and the object, outermost first.
    let mut containers = Vec::new();
    for (i, n) in chain.iter().enumerate().skip(1) {
        if matches!(n.role, Role::Window | Role::Dialog | Role::Application) {
            break;
        }
        if is_container(n.role) && (detailed || i == 1 || is_collection(n.role)) {
            containers.push((i, n));
        }
    }
    for (i, n) in containers.iter().rev() {
        let mut s = String::new();
        if let Some(name) = named(n) {
            s.push_str(name);
            s.push(' ');
        }
        s.push_str(n.role.spoken());
        // Position of the item inside this container: the previous node in the chain.
        if let Some(child) = chain.get(i - 1)
            && let Some((at, of)) = position(n, child.key)
        {
            let _ = write!(s, ", {at} of {of}");
        }
        parts.push(s);
    }
    // The object.
    let mut obj = String::new();
    if let Some(n) = named(leaf) {
        obj.push_str(n);
        obj.push(' ');
    }
    obj.push_str(leaf.role.spoken());
    let obj = obj.trim().to_owned();
    if !obj.is_empty() {
        parts.push(obj);
    }
    let states = state_words(leaf.states);
    if !states.is_empty() {
        parts.push(states.join(" "));
    }
    if let Some(v) = leaf.value.as_ref().and_then(value_text) {
        parts.push(v);
    }
    if detailed {
        if let Some(d) = leaf.description.as_deref().filter(|d| !d.is_empty()) {
            parts.push(d.to_owned());
        }
        if matches!(leaf.fresh, Freshness::Stale { .. } | Freshness::Defunct) {
            parts.push("information may be out of date".to_owned());
        }
    }
    parts.join(", ")
}

/// Title of the window the node is in; `None` when the mirror does not know the chain up to it.
pub(crate) fn window_title(chain: &[Arc<NodeSnapshot>]) -> Option<String> {
    let w = chain
        .iter()
        .find(|n| matches!(n.role, Role::Window | Role::Dialog))?;
    Some(named(w).unwrap_or(w.role.spoken()).to_owned())
}

pub(crate) fn node_info(chain: &[Arc<NodeSnapshot>]) -> String {
    let Some(n) = chain.first() else {
        return "no focus".to_owned();
    };
    let fresh = match n.fresh {
        Freshness::Fresh { .. } => "fresh",
        Freshness::Stale { .. } => "stale",
        Freshness::Defunct => "defunct",
    };
    let states = state_words(n.states);
    format!(
        "node {} of application {}, role {}, {}, {} ancestors known, {} states, name {}",
        n.key.local.0,
        n.key.app.serial,
        if n.role.spoken().is_empty() {
            "unknown"
        } else {
            n.role.spoken()
        },
        fresh,
        chain.len() - 1,
        if states.is_empty() {
            "no listed".to_owned()
        } else {
            states.join(" ")
        },
        named(n).unwrap_or("none"),
    )
}

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const DAYS: [&str; 7] = [
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
];

pub(crate) fn time_text(t: LocalTime, clock_24h: bool) -> String {
    if clock_24h {
        format!("{}:{:02}", t.hour, t.minute)
    } else {
        let (h, ap) = match t.hour {
            0 => (12, "AM"),
            1..=11 => (t.hour, "AM"),
            12 => (12, "PM"),
            h => (h - 12, "PM"),
        };
        format!("{h}:{:02} {ap}", t.minute)
    }
}

pub(crate) fn date_text(t: LocalTime) -> String {
    let day = DAYS.get(usize::from(t.weekday)).copied().unwrap_or("");
    let month = MONTHS
        .get(usize::from(t.month.saturating_sub(1)))
        .copied()
        .unwrap_or("");
    format!("{day}, {month} {}, {}", t.day, t.year)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gorca_model::{AppId, AttrMap, ConnectorId, Ifaces, LocalId, Relations};
    use std::time::Instant;

    fn key(n: u64) -> NodeKey {
        NodeKey {
            app: AppId {
                connector: ConnectorId(1),
                serial: 3,
            },
            local: LocalId(n),
        }
    }

    fn node(n: u64, role: Role, name: Option<&str>, parent: Option<u64>) -> NodeSnapshot {
        NodeSnapshot {
            key: key(n),
            role,
            states: StateSet::empty(),
            name: name.map(Arc::from),
            description: None,
            value: None,
            parent: parent.map(key),
            children: ChildList::Unknown,
            interfaces: Ifaces::empty(),
            attrs: AttrMap::default(),
            relations: Relations::default(),
            bounds: None,
            have: gorca_model::PropMask::PRESENT,
            confirmed_seq: 0,
            fresh: Freshness::Fresh { at: Instant::now() },
        }
    }

    /// app(1) > window(2) > list(3, 12 known children) > item(7 = third child)
    fn chain() -> Vec<Arc<NodeSnapshot>> {
        let item = node(7, Role::ListItem, Some("Inbox"), Some(3));
        let mut list = node(3, Role::List, Some("Folders"), Some(2));
        list.children = ChildList::Known(
            (5..17)
                .map(|i| if i == 7 { key(7) } else { key(100 + i) })
                .collect::<smallvec::SmallVec<[NodeKey; 8]>>(),
        );
        // Move the item to position 3: children 5..17 are ids, item key(7) is at index 2.
        let win = node(2, Role::Window, Some("Mail"), Some(1));
        let app = node(1, Role::Application, Some("Evolution"), None);
        vec![item, list, win, app]
            .into_iter()
            .map(Arc::new)
            .collect()
    }

    #[test]
    fn basic_where_am_i_names_app_window_container_and_object() {
        let t = where_am_i(&chain(), 1, Verbosity::Normal);
        assert_eq!(
            t,
            "Evolution, Mail window, Folders list, 3 of 12, Inbox list item"
        );
    }

    #[test]
    fn brief_verbosity_drops_the_application() {
        let t = where_am_i(&chain(), 1, Verbosity::Brief);
        assert!(t.starts_with("Mail window"), "{t}");
    }

    #[test]
    fn detailed_adds_description_and_states() {
        let mut c = chain();
        let mut item = (*c[0]).clone();
        item.description = Some(Arc::from("Unread messages"));
        item.states = StateSet::SELECTED | StateSet::FOCUSED;
        c[0] = Arc::new(item);
        let t = where_am_i(&c, 2, Verbosity::Normal);
        assert!(
            t.ends_with("Inbox list item, selected, Unread messages"),
            "{t}"
        );
        let basic = where_am_i(&c, 1, Verbosity::Normal);
        assert!(!basic.contains("Unread"));
    }

    #[test]
    fn third_press_spells_the_name() {
        assert_eq!(where_am_i(&chain(), 3, Verbosity::Normal), "I n b o x");
        let c = vec![Arc::new(node(1, Role::Button, None, None))];
        assert_eq!(where_am_i(&c, 3, Verbosity::Normal), "button, no name");
        assert_eq!(spell("a b"), "a space b");
    }

    #[test]
    fn partial_chains_still_speak() {
        let c = vec![Arc::new(node(7, Role::Button, Some("OK"), Some(2)))];
        assert_eq!(where_am_i(&c, 1, Verbosity::Normal), "OK button");
        assert_eq!(where_am_i(&[], 1, Verbosity::Normal), "no focus");
    }

    #[test]
    fn window_title_finds_the_enclosing_window_or_dialog() {
        assert_eq!(window_title(&chain()).as_deref(), Some("Mail"));
        assert_eq!(window_title(&chain()[..1]), None);
    }

    #[test]
    fn time_and_date_follow_the_clock_format() {
        let t = LocalTime {
            year: 2026,
            month: 10,
            day: 13,
            weekday: 1,
            hour: 14,
            minute: 5,
        };
        assert_eq!(time_text(t, true), "14:05");
        assert_eq!(time_text(t, false), "2:05 PM");
        assert_eq!(time_text(LocalTime { hour: 0, ..t }, false), "12:05 AM");
        assert_eq!(date_text(t), "Tuesday, October 13, 2026");
    }

    #[test]
    fn node_info_lists_identity_and_freshness() {
        let s = node_info(&chain());
        assert!(
            s.contains("node 7 of application 3") && s.contains("fresh"),
            "{s}"
        );
    }
}
