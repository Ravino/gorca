// SPDX-License-Identifier: GPL-3.0-or-later
//! Sweep over every recorded node of the A2 corpus (all pages, including the M8 pages that have
//! no `expect.toml` row): the normaliser is total, deterministic and keeps its invariants, and the
//! Unknown/Absent split follows the fetched mask. Prints the M-10 unmapped list seen on the corpus.

mod common;

use std::collections::BTreeMap;

use common::{Rec, load, root};
use gorca_model::PropMask;
use gorca_model::sem::{Check, Expand, Group, Invalid, Press, SemRole};
use gorca_sem_atspi::{Notes, RawNode, SrcRecord, normalise};

fn all_nodes() -> Vec<(String, String, Rec)> {
    let mut out = Vec::new();
    for engine in ["firefox", "chromium", "gtk4"] {
        let Ok(dir) = std::fs::read_dir(root().join(engine)) else {
            continue;
        };
        let mut pages: Vec<String> = dir
            .filter_map(Result::ok)
            .filter_map(|e| {
                e.file_name()
                    .to_str()
                    .and_then(|n| n.strip_suffix(".jsonl"))
                    .map(str::to_owned)
            })
            .collect();
        pages.sort();
        for page in pages {
            for rec in load(engine, &page).unwrap_or_default() {
                out.push((engine.to_owned(), page.clone(), rec));
            }
        }
    }
    out
}

fn norm(r: &Rec, fetched: PropMask) -> SrcRecord {
    let attrs: Vec<(&str, &str)> = r
        .attrs
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    normalise(&RawNode {
        fetched,
        role: r.role,
        states: r.states,
        attrs: &attrs,
        name: Some(r.name.as_str()),
        description: None,
    })
}

fn everything() -> PropMask {
    PropMask::ROLE | PropMask::STATES | PropMask::ATTRS | PropMask::NAME | PropMask::DESCRIPTION
}

#[test]
fn invariants_hold_on_every_recorded_node() {
    let nodes = all_nodes();
    assert!(nodes.len() > 3000, "corpus shrank: {} nodes", nodes.len());

    let mut unmapped_engine: BTreeMap<u32, u32> = BTreeMap::new();
    let mut note_counts: BTreeMap<&'static str, u32> = BTreeMap::new();
    let mut unknown_role = 0u32;
    for (engine, page, r) in &nodes {
        let rec = norm(r, everything());
        let ctx = format!("{engine}/{page} role {}", r.role);
        // Deterministic.
        assert_eq!(rec, norm(r, everything()), "{ctx}");
        // Everything fetched: every group of the A1 registry the normaliser feeds is known.
        for g in [
            Group::Role,
            Group::Name,
            Group::Description,
            Group::Check,
            Group::Press,
            Group::Expand,
            Group::Select,
            Group::Current,
            Group::Invalid,
            Group::Popup,
            Group::Avail,
            Group::Flags,
            Group::Level,
            Group::SetPos,
            Group::Grid,
            Group::Live,
            Group::Orientation,
            Group::Sort,
        ] {
            assert!(
                rec.known.has(g),
                "{ctx}: {g:?} unknown with everything fetched"
            );
        }
        // SM-2: exclusive states cannot coexist.
        assert!(
            rec.sem.check == Check::NotCheckable || rec.sem.press == Press::NotToggle,
            "{ctx}: both checked and pressed"
        );
        // Blink exports level=1 on list items; it must not leak.
        if rec.sem.level > 0 {
            assert!(
                matches!(
                    rec.sem.role,
                    SemRole::Heading
                        | SemRole::TreeItem
                        | SemRole::Row
                        | SemRole::Comment
                        | SemRole::StaticText
                ),
                "{ctx}: level on {:?}",
                rec.sem.role
            );
        }
        // INVALID_ENTRY is never dropped (P-1): without an exported kind it is `Invalid::True`,
        // including on required web fields, where the note marks the empty-required ambiguity.
        if r.states.has(gorca_sem_atspi::states::bit::INVALID_ENTRY)
            && !r.attrs.iter().any(|(k, _)| k == "invalid")
        {
            assert_eq!(rec.sem.invalid, Invalid::True, "{ctx}");
        }
        // Set positions are 1-based.
        if let Some(p) = rec.sem.pos {
            assert!(p.pos >= 1, "{ctx}");
        }
        // Expanded state without the expandable bit is still canonical.
        if rec.sem.expand == Expand::Expanded {
            assert!(
                r.states.has(gorca_sem_atspi::states::bit::EXPANDED),
                "{ctx}"
            );
        }
        // An unmapped engine role is only ever Unknown when no token decided.
        if rec.notes.contains(Notes::UNMAPPED_ENGINE_ROLE) {
            *unmapped_engine.entry(r.role).or_default() += 1;
        }
        if rec.sem.role == SemRole::Unknown {
            unknown_role += 1;
        }
        for (flag, name) in [
            (Notes::INVALID_ROLE_TOKEN, "invalid-role-token"),
            (Notes::UNMAPPED_ROLE_TOKEN, "unmapped-role-token"),
            (Notes::INVALID_CURRENT_TOKEN, "invalid-current-token"),
            (
                Notes::INVALID_ENTRY_ON_REQUIRED,
                "invalid-entry-on-required",
            ),
            (Notes::MALFORMED_NUMBER, "malformed-number"),
            (Notes::TAG_TRUNCATED, "tag-truncated"),
        ] {
            if rec.notes.contains(flag) {
                *note_counts.entry(name).or_default() += 1;
            }
        }
    }
    eprintln!(
        "sweep: {} nodes; Unknown role {unknown_role}; unmapped engine roles seen {unmapped_engine:?}; notes {note_counts:?}",
        nodes.len()
    );
    // Roles the corpus never produces as Unknown except the unreadable ones.
    assert!(
        unknown_role as usize <= nodes.len() / 100,
        "{unknown_role} Unknown roles"
    );
}

#[test]
fn unknown_vs_absent_follows_the_fetched_mask_on_real_nodes() {
    let nodes = all_nodes();
    let subsets = [
        PropMask::ROLE,
        PropMask::ROLE | PropMask::STATES,
        PropMask::ROLE | PropMask::ATTRS,
        PropMask::STATES | PropMask::ATTRS,
        PropMask::NAME | PropMask::DESCRIPTION,
        PropMask::CACHE_ITEM,
        PropMask::empty(),
    ];
    for (engine, page, r) in nodes.iter().step_by(7) {
        for f in subsets {
            let rec = norm(r, f);
            let ctx = format!("{engine}/{page} role {} fetched {f:?}", r.role);
            let need = |g: Group, want: bool| {
                assert_eq!(rec.known.has(g), want, "{ctx}: {g:?}");
            };
            let (role, states, attrs) = (
                f.contains(PropMask::ROLE),
                f.contains(PropMask::STATES),
                f.contains(PropMask::ATTRS),
            );
            need(Group::Role, role);
            need(Group::Name, f.contains(PropMask::NAME));
            need(Group::Description, f.contains(PropMask::DESCRIPTION));
            need(Group::Check, role && states);
            need(Group::Press, role && states);
            need(Group::Select, role && states);
            need(Group::Avail, role && states);
            need(Group::Expand, states);
            need(Group::Orientation, states);
            need(Group::Flags, states);
            need(Group::Invalid, states);
            need(Group::Current, attrs);
            need(Group::Sort, attrs);
            need(Group::SetPos, attrs);
            need(Group::Grid, attrs);
            need(Group::Level, role && attrs);
            need(Group::Live, role && attrs);
            // An unknown group never carries a value: it holds the default.
            if !rec.known.has(Group::Role) {
                assert_eq!(rec.sem.role, SemRole::Unknown, "{ctx}");
            }
            if !rec.known.has(Group::Name) {
                assert_eq!(rec.name, None, "{ctx}");
            }
        }
    }
}
