// SPDX-License-Identifier: GPL-3.0-or-later
//! Text of the `info.*` and `debug.*` commands: pure functions over mirror snapshots.
//!
//! "Where am I" v1 (ADR-0005 6.4) uses the ancestry the mirror already holds: application,
//! window, the container chain with positions ("list, 3 of 12"), then the object itself.

use gorca_i18n::{MsgId as M, msg, t as tr};
use std::fmt::Write as _;
use std::sync::Arc;

use gorca_config::Verbosity;
use gorca_model::sem::SemRole;
use gorca_model::{ChildList, Freshness, NodeKey, NodeSnapshot};

use super::state::LocalTime;
use crate::directory::MirrorDirectory;
use crate::nodekind::{is_app_node, is_window_role, role_of};

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

/// What the fused entry of one node says about it (`Reason::WhereAmI`, M22b4): the words of the
/// ancestry answer. Built by the Reader from the node's entry through `gorca-present`; this
/// module only arranges them.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Words {
    /// The name, `None` when the node has none worth saying.
    pub name: Option<String>,
    /// The role word(s), joined by a space.
    pub role_text: String,
    /// The state words.
    pub state_text: Vec<String>,
    /// The value (and the contents of a field).
    pub values: Vec<String>,
    /// The description, from the entry.
    pub description: Option<String>,
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
fn is_collection(role: SemRole) -> bool {
    matches!(
        role,
        SemRole::List
            | SemRole::Listbox
            | SemRole::TabList
            | SemRole::Menu
            | SemRole::Menubar
            | SemRole::Tree
            | SemRole::Toolbar
            | SemRole::Table
            | SemRole::Grid
            | SemRole::TreeGrid
            | SemRole::Row
    )
}

/// Roles that are worth naming in the container chain.
fn is_container(role: SemRole) -> bool {
    is_collection(role)
        || matches!(
            role,
            SemRole::Generic
                | SemRole::Group
                | SemRole::Document
                | SemRole::DocumentWeb
                | SemRole::Region
                | SemRole::Banner
                | SemRole::Complementary
                | SemRole::ContentInfo
                | SemRole::Main
                | SemRole::Navigation
                | SemRole::Search
                | SemRole::Form
                | SemRole::RadioGroup
                | SemRole::Figure
                | SemRole::Presentation
                | SemRole::Log
                | SemRole::Timer
                | SemRole::RowGroup
                | SemRole::Feed
                | SemRole::Comment
                | SemRole::Popover
                | SemRole::Status
        )
}

/// Which parts of the answer a node plays in "where am I" (the recorded-corpus goldens read it):
/// a collection, a named container, a window, the application.
pub(crate) fn shape_of(n: &NodeSnapshot) -> (bool, bool, bool, bool) {
    let role = role_of(n);
    (
        is_collection(role),
        is_container(role),
        is_window_role(role),
        is_app_node(n),
    )
}

/// Spell a text out: `Save` becomes "S a v e" (the speech service reads single letters).
pub(crate) fn spell(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c == ' ' {
                tr(M::WordSpace).to_owned()
            } else {
                c.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Text of "where am I". `chain` is leaf first and `words` has the words of each node of it.
/// `repeat`: 1 basic, 2 detailed, 3 spell the name.
pub(crate) fn where_am_i_parts(
    chain: &[Arc<NodeSnapshot>],
    words: &[Words],
    repeat: u8,
    level: Verbosity,
) -> Vec<String> {
    let (Some(leaf), Some(lw)) = (chain.first(), words.first()) else {
        return vec![tr(M::NoFocus).to_owned()];
    };
    if repeat >= 3 {
        return match lw.name.as_deref() {
            Some(n) => vec![spell(n)],
            None => vec![msg!(InfoNoName, role = lw.role_text.as_str())],
        };
    }
    let detailed = repeat == 2 || level == Verbosity::Verbose;
    let mut parts: Vec<String> = Vec::new();
    // Application and window.
    let app = chain.iter().zip(words).rev().find(|(n, _)| is_app_node(n));
    if let Some(a) = app.and_then(|(_, w)| w.name.as_deref())
        && level != Verbosity::Brief
    {
        parts.push(a.to_owned());
    }
    let window = chain
        .iter()
        .zip(words)
        .find(|(n, _)| is_window_role(role_of(n)));
    if let Some((_, w)) = window {
        parts.push(match w.name.as_deref() {
            Some(n) => msg!(InfoWindow, name = n, role = w.role_text.as_str()),
            None => w.role_text.clone(),
        });
    }
    // Container chain between the window and the object, outermost first.
    let mut containers = Vec::new();
    for (i, n) in chain.iter().enumerate().skip(1) {
        if is_window_role(role_of(n)) || is_app_node(n) {
            break;
        }
        if is_container(role_of(n)) && (detailed || i == 1 || is_collection(role_of(n))) {
            containers.push((i, n));
        }
    }
    for (i, n) in containers.iter().rev() {
        let mut s = String::new();
        let Some(w) = words.get(*i) else { continue };
        if let Some(name) = w.name.as_deref() {
            s.push_str(name);
            s.push(' ');
        }
        s.push_str(&w.role_text);
        // Position of the item inside this container: the previous node in the chain.
        if let Some(child) = chain.get(i - 1)
            && let Some((at, of)) = position(n, child.key)
        {
            let _ = write!(s, ", {}", msg!(InfoPosition, at = at, of = of));
        }
        // A container with no name and no role word says nothing (no lone ", 3 of 12").
        let s = s.trim_end().trim_start_matches(", ");
        if !s.is_empty() {
            parts.push(s.to_owned());
        }
    }
    // The object.
    let mut obj = String::new();
    if let Some(n) = lw.name.as_deref() {
        obj.push_str(n);
        obj.push(' ');
    }
    obj.push_str(&lw.role_text);
    let obj = obj.trim().to_owned();
    if !obj.is_empty() {
        parts.push(obj);
    }
    if !lw.state_text.is_empty() {
        parts.push(lw.state_text.join(" "));
    }
    parts.extend(lw.values.iter().cloned());
    if detailed {
        if let Some(d) = lw.description.as_deref().filter(|d| !d.is_empty()) {
            parts.push(d.to_owned());
        }
        if matches!(leaf.fresh, Freshness::Stale { .. } | Freshness::Defunct) {
            parts.push(tr(M::InfoOutOfDate).to_owned());
        }
    }
    parts
}

pub(crate) fn where_am_i(
    chain: &[Arc<NodeSnapshot>],
    words: &[Words],
    repeat: u8,
    level: Verbosity,
) -> String {
    where_am_i_parts(chain, words, repeat, level).join(", ")
}

/// Title of the window the node is in; `None` when the mirror does not know the chain up to it.
pub(crate) fn window_title(chain: &[Arc<NodeSnapshot>], words: &[Words]) -> Option<String> {
    let (_, w) = chain
        .iter()
        .zip(words)
        .find(|(n, _)| is_window_role(role_of(n)))?;
    Some(w.name.clone().unwrap_or_else(|| w.role_text.clone()))
}

pub(crate) fn node_info(chain: &[Arc<NodeSnapshot>], words: &[Words]) -> String {
    let (Some(n), Some(w)) = (chain.first(), words.first()) else {
        return tr(M::NoFocus).to_owned();
    };
    let fresh = match n.fresh {
        Freshness::Fresh { .. } => "fresh",
        Freshness::Stale { .. } => "stale",
        Freshness::Defunct => "defunct",
    };
    format!(
        "node {} of application {}, role {}, {}, {} ancestors known, {} states, name {}",
        n.key.local.0,
        n.key.app.serial,
        if w.role_text.is_empty() {
            "unknown"
        } else {
            w.role_text.as_str()
        },
        fresh,
        chain.len() - 1,
        if w.state_text.is_empty() {
            "no listed".to_owned()
        } else {
            w.state_text.join(" ")
        },
        w.name.as_deref().unwrap_or("none"),
    )
}

const MONTHS: [M; 12] = [
    M::Month1,
    M::Month2,
    M::Month3,
    M::Month4,
    M::Month5,
    M::Month6,
    M::Month7,
    M::Month8,
    M::Month9,
    M::Month10,
    M::Month11,
    M::Month12,
];
const DAYS: [M; 7] = [
    M::Day1,
    M::Day2,
    M::Day3,
    M::Day4,
    M::Day5,
    M::Day6,
    M::Day7,
];

pub(crate) fn time_text(t: LocalTime, clock_24h: bool) -> String {
    let m = format!("{:02}", t.minute);
    if clock_24h {
        msg!(Time24h, h = t.hour, m = m.as_str())
    } else {
        // The period word follows the time of day (Russian: ночи, утра, дня, вечера); English
        // keeps AM and PM.
        let (h, ap) = match t.hour {
            0 => (12, M::TimeNight),
            1..=3 => (t.hour, M::TimeNight),
            4..=11 => (t.hour, M::TimeMorning),
            12 => (12, M::TimeDay),
            13..=16 => (t.hour - 12, M::TimeDay),
            h => (h - 12, M::TimeEvening),
        };
        msg!(Time12h, h = h, m = m.as_str(), ap = gorca_i18n::t(ap))
    }
}

pub(crate) fn date_text(t: LocalTime) -> String {
    let day = DAYS.get(usize::from(t.weekday)).map_or("", |d| tr(*d));
    let month = MONTHS
        .get(usize::from(t.month.saturating_sub(1)))
        .map_or("", |m| tr(*m));
    msg!(DateFull, day = day, month = month, d = t.day, y = t.year)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gorca_model::{AppId, AttrMap, ConnectorId, Ifaces, LocalId, Relations};
    use gorca_model::{Role, StateSet};
    use std::time::Instant;

    fn at(hour: u8, minute: u8) -> LocalTime {
        LocalTime {
            year: 2026,
            month: 10,
            day: 17,
            weekday: 5,
            hour,
            minute,
        }
    }

    #[test]
    fn the_russian_period_word_follows_the_time_of_day() {
        let _ru = gorca_i18n::scoped(gorca_i18n::Lang::Ru);
        for (h, want) in [
            (0, "12:07 ночи"),
            (3, "3:07 ночи"),
            (4, "4:07 утра"),
            (11, "11:07 утра"),
            (12, "12:07 дня"),
            (15, "3:07 дня"),
            (16, "4:07 дня"),
            (17, "5:07 вечера"),
            (23, "11:07 вечера"),
        ] {
            assert_eq!(time_text(at(h, 7), false), want, "{h}");
        }
    }

    #[test]
    fn english_keeps_am_and_pm() {
        let _en = gorca_i18n::scoped(gorca_i18n::Lang::En);
        for (h, want) in [
            (0, "12:07 AM"),
            (4, "4:07 AM"),
            (12, "12:07 PM"),
            (23, "11:07 PM"),
        ] {
            assert_eq!(time_text(at(h, 7), false), want, "{h}");
        }
    }

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
            sem: gorca_model::NodeSem::NONE,
        }
        .with_test_record()
    }

    /// The words of each node of `chain` from the entry its source record gives (as the Reader
    /// does for a node it holds only as a record).
    fn words(chain: &[Arc<NodeSnapshot>]) -> Vec<Words> {
        use gorca_present::{ComposeCtx, ReportFlags};
        chain
            .iter()
            .map(|n| {
                let n = (**n).clone().with_test_record();
                let entry = n.sem.rec.as_ref().map(|r| r.provisional_entry()).unwrap();
                let ctx = ComposeCtx {
                    flags: ReportFlags::default(),
                    may_refine: false,
                    web: false,
                    skip_selected: false,
                    hedge_inferred: false,
                    lang: gorca_i18n::language(),
                };
                crate::present::node::words_of(
                    &entry,
                    &n,
                    ctx,
                    gorca_present::RenderOpts::default(),
                )
            })
            .collect()
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
        let t = where_am_i(&chain(), &words(&chain()), 1, Verbosity::Normal);
        assert_eq!(
            t,
            "Evolution, Mail window, Folders list, 3 of 12, Inbox list item"
        );
    }

    #[test]
    fn brief_verbosity_drops_the_application() {
        let t = where_am_i(&chain(), &words(&chain()), 1, Verbosity::Brief);
        assert!(t.starts_with("Mail window"), "{t}");
    }

    #[test]
    fn detailed_adds_description_and_states() {
        let mut c = chain();
        let mut item = (*c[0]).clone();
        item.description = Some(Arc::from("Unread messages"));
        item.states = StateSet::SELECTED | StateSet::FOCUSED;
        c[0] = Arc::new(item);
        let t = where_am_i(&c, &words(&c), 2, Verbosity::Normal);
        assert!(
            t.ends_with("Inbox list item, selected, Unread messages"),
            "{t}"
        );
        let basic = where_am_i(&c, &words(&c), 1, Verbosity::Normal);
        assert!(!basic.contains("Unread"));
    }

    #[test]
    fn third_press_spells_the_name() {
        assert_eq!(
            where_am_i(&chain(), &words(&chain()), 3, Verbosity::Normal),
            "I n b o x"
        );
        let c = vec![Arc::new(node(1, Role::Button, None, None))];
        assert_eq!(
            where_am_i(&c, &words(&c), 3, Verbosity::Normal),
            "button, no name"
        );
        assert_eq!(spell("a b"), "a space b");
    }

    #[test]
    fn partial_chains_still_speak() {
        let c = vec![Arc::new(node(7, Role::Button, Some("OK"), Some(2)))];
        assert_eq!(
            where_am_i(&c, &words(&c), 1, Verbosity::Normal),
            "OK button"
        );
        assert_eq!(where_am_i(&[], &[], 1, Verbosity::Normal), "no focus");
    }

    #[test]
    fn window_title_finds_the_enclosing_window_or_dialog() {
        assert_eq!(
            window_title(&chain(), &words(&chain())).as_deref(),
            Some("Mail")
        );
        assert_eq!(window_title(&chain()[..1], &words(&chain()[..1])), None);
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
        let s = node_info(&chain(), &words(&chain()));
        assert!(
            s.contains("node 7 of application 3") && s.contains("fresh"),
            "{s}"
        );
    }

    #[test]
    fn time_and_date_follow_the_ui_language() {
        let t = LocalTime {
            year: 2026,
            month: 10,
            day: 17,
            weekday: 5,
            hour: 15,
            minute: 7,
        };
        assert_eq!(time_text(t, true), "15:07");
        assert_eq!(time_text(t, false), "3:07 PM");
        assert_eq!(date_text(t), "Saturday, October 17, 2026");
        let _ru = gorca_i18n::scoped(gorca_i18n::Lang::Ru);
        assert_eq!(time_text(t, true), "15:07");
        assert_eq!(time_text(t, false), "3:07 дня");
        assert_eq!(date_text(t), "суббота, 17 октября 2026 года");
    }
}
