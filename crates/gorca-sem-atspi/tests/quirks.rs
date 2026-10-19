// SPDX-License-Identifier: GPL-3.0-or-later
//! The shipped quirk, PI, `inexpressible` and selector tables load through the `gorca-fuse`
//! validator and do what the A2 corpus says they should (M22a-B5).

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;

use common::{Rec, load, root, run};
use gorca_fuse::repair::{ix, unknown_eff};
use gorca_fuse::strata::{Quirks, Record};
use gorca_fuse::value::{Slot, Val};
use gorca_fuse::{
    AppFacts, PrimarySlot, Profile, ProfileSelector, QuirkSet, Rules, parse_inexpressible, parse_pi,
};
use gorca_model::sem::{GROUP_COUNT, Group, SemRole};
use gorca_sem_atspi::roles::pick_token;
use gorca_sem_atspi::tables::{BUNDLES, DEFAULT_PROFILE, SELECTOR, bundle};
use gorca_sem_atspi::{RoleSource, SrcRecord};

fn pages(engine: &str) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(root().join(engine))
        .map(|d| {
            d.filter_map(Result::ok)
                .filter_map(|e| {
                    e.file_name()
                        .to_str()
                        .and_then(|n| n.strip_suffix(".jsonl"))
                        .map(str::to_owned)
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

#[allow(clippy::expect_used)]
fn quirks_of(name: &str) -> QuirkSet {
    QuirkSet::parse(bundle(name).expect("bundle").quirks).expect("quirks load")
}

/// The S-1 record a connector would hand the quirk stratum for this node.
fn record_of(n: &SrcRecord) -> Record {
    let mut slots = unknown_eff();
    slots[ix(Group::Role)] = Slot::Val(Val::Role(n.sem.role));
    if let Some(name) = &n.name {
        slots[ix(Group::Name)] = Slot::Val(Val::Name(Arc::clone(name)));
    }
    if let Some(p) = n.sem.pos {
        slots[ix(Group::SetPos)] = Slot::Val(Val::SetPos(p));
    }
    Record {
        slots,
        hit: [0; GROUP_COUNT],
    }
}

fn name_of(r: &Record) -> Option<String> {
    match &r.slots[ix(Group::Name)] {
        Slot::Val(Val::Name(n)) => Some(n.to_string()),
        _ => None,
    }
}

#[test]
fn every_shipped_table_loads_and_builds_a_profile() {
    for b in &BUNDLES {
        let q = QuirkSet::parse(b.quirks).unwrap_or_else(|e| panic!("{} quirks: {e}", b.name));
        let pi = parse_pi(b.pi).unwrap_or_else(|e| panic!("{} pi: {e}", b.name));
        let inex = parse_inexpressible(b.inexpressible)
            .unwrap_or_else(|e| panic!("{} inexpressible: {e}", b.name));
        let mut p = Profile::new(&[PrimarySlot::P0], Rules::aria());
        pi.apply(&mut p);
        p.inexpressible = inex;
        assert_eq!(
            p.pi(Group::Name),
            &[PrimarySlot::P0, PrimarySlot::P1],
            "{}",
            b.name
        );
        match b.name {
            "gecko" | "blink" => assert!(p.inexpressible.has(Group::Invalid), "{}", b.name),
            _ => assert!(p.inexpressible.is_empty(), "{}", b.name),
        }
        assert!(!p.inexpressible.has(Group::Role), "{}", b.name);
        assert_eq!(
            q.is_empty(),
            b.name != "gecko",
            "only Gecko ships rewrite rows today"
        );
    }
    // The selector only names shipped profiles.
    let s = ProfileSelector::parse(SELECTOR).expect("selector");
    let facts = |n, t, w| AppFacts {
        app_name: n,
        toolkit: t,
        web_engine: w,
    };
    for (f, want) in [
        (facts("Firefox", None, None), Some("gecko")),
        (facts("anything", None, Some("Gecko")), Some("gecko")),
        (facts("chromium", None, None), Some("blink")),
        (facts("Google-Chrome", None, None), Some("blink")),
        (facts("slack", None, Some("blink")), Some("blink")),
        (facts("gnome-calculator", Some("GTK"), None), Some("gtk4")),
        (facts("orca", None, None), None),
    ] {
        assert_eq!(s.select(&f), want, "{f:?}");
        if let Some(n) = s.select(&f) {
            assert!(bundle(n).is_some(), "selector names unknown profile {n}");
        }
    }
    assert!(bundle(DEFAULT_PROFILE).is_some());
}

/// The Gecko list-marker row turns Gecko's names into the names Blink exports for the same
/// fixture element, on every page both engines recorded.
#[test]
fn gecko_list_marker_row_reproduces_the_blink_names() {
    let gecko = quirks_of("gecko");
    let blink = quirks_of("blink");
    let (mut stripped, mut untouched_gecko, mut blink_changed) = (0u32, 0u32, 0u32);
    let mut mismatches = Vec::new();
    for page in pages("firefox") {
        let ff = load("firefox", &page).unwrap_or_default();
        let ch = load("chromium", &page).unwrap_or_default();
        let ch_names: BTreeMap<&str, &Rec> = ch
            .iter()
            .filter_map(|r| r.fixture_id.as_deref().map(|id| (id, r)))
            .collect();
        for r in &ff {
            let n = run("firefox", r);
            let mut rec = record_of(&n);
            let before = name_of(&rec);
            gecko.apply(PrimarySlot::P0, &mut rec);
            let after = name_of(&rec);
            if rec.hit[ix(Group::Name)] == 1 {
                stripped += 1;
                assert_ne!(before, after);
                // Same element in Blink: same name.
                if let Some(c) = r.fixture_id.as_deref().and_then(|id| ch_names.get(id))
                    && !c.name.is_empty()
                    && Some(c.name.clone()) != after
                {
                    mismatches.push(format!(
                        "{page}/{:?}: {:?} vs blink {:?}",
                        r.fixture_id, after, c.name
                    ));
                }
            } else {
                untouched_gecko += 1;
                assert_eq!(before, after);
            }
        }
        for r in &ch {
            let n = run("chromium", r);
            // Blink names never carry a marker: the Gecko row must not fire on them either.
            let mut rec = record_of(&n);
            gecko.apply(PrimarySlot::P0, &mut rec);
            assert_eq!(rec.hit[ix(Group::Name)], 0, "{page}: {:?}", r.fixture_id);
            // And the Blink table rewrites nothing.
            let mut rec = record_of(&n);
            let snapshot = rec.slots.clone();
            blink.apply(PrimarySlot::P0, &mut rec);
            if rec.slots != snapshot {
                blink_changed += 1;
            }
        }
    }
    eprintln!(
        "list-marker: {stripped} names stripped, {untouched_gecko} other Gecko nodes untouched, {blink_changed} Blink rewrites"
    );
    assert!(stripped >= 20, "only {stripped} marker names on the corpus");
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
    assert_eq!(blink_changed, 0);
}

#[test]
fn gecko_menu_positions_become_unknown() {
    let gecko = quirks_of("gecko");
    let blink = quirks_of("blink");
    let (mut menu, mut other_kept) = (0u32, 0u32);
    for page in pages("firefox") {
        for r in load("firefox", &page).unwrap_or_default() {
            let n = run("firefox", &r);
            let mut rec = record_of(&n);
            gecko.apply(PrimarySlot::P0, &mut rec);
            let is_menu = matches!(
                n.sem.role,
                SemRole::MenuItem | SemRole::MenuItemCheckbox | SemRole::MenuItemRadio
            );
            if is_menu && n.sem.pos.is_some() {
                menu += 1;
                assert!(
                    rec.slots[ix(Group::SetPos)].is_unknown(),
                    "{page} {:?}",
                    r.fixture_id
                );
                assert_eq!(rec.hit[ix(Group::SetPos)], 2);
            } else if n.sem.pos.is_some() {
                other_kept += 1;
                assert!(!rec.slots[ix(Group::SetPos)].is_unknown());
            }
        }
    }
    assert!(
        menu >= 5 && other_kept >= 50,
        "menu {menu}, other {other_kept}"
    );
    // Blink counts positions over the whole menu: kept.
    for page in pages("chromium") {
        for r in load("chromium", &page).unwrap_or_default() {
            let n = run("chromium", &r);
            let mut rec = record_of(&n);
            blink.apply(PrimarySlot::P0, &mut rec);
            assert_eq!(
                rec.slots[ix(Group::SetPos)].is_unknown(),
                n.sem.pos.is_none()
            );
        }
    }
}

/// Blink exports `xml-roles` only for authored roles and landmarks, and trusting it is right:
/// wherever a token maps, the resolved role is that token (the two documented exceptions are the
/// presentational conflict and the password field).
#[test]
fn blink_xml_roles_are_trusted() {
    let (mut trusted, mut overridden) = (0u32, 0u32);
    for page in pages("chromium") {
        for r in load("chromium", &page).unwrap_or_default() {
            let Some((_, v)) = r.attrs.iter().find(|(k, _)| k == "xml-roles") else {
                continue;
            };
            let Some(want) = pick_token(v).role else {
                continue;
            };
            let n = run("chromium", &r);
            match n.role_source {
                RoleSource::Token => {
                    assert_eq!(n.sem.role, want, "{page} {:?} token {v}", r.fixture_id);
                    trusted += 1;
                }
                RoleSource::EngineOverToken => {
                    assert!(
                        matches!(want, SemRole::Presentation | SemRole::Generic)
                            || n.sem.role == SemRole::PasswordBox
                            || n.sem.role == SemRole::DocumentWeb,
                        "{page} {:?}: token {v} lost to {:?}",
                        r.fixture_id,
                        n.sem.role
                    );
                    overridden += 1;
                }
                RoleSource::Engine => {
                    panic!("{page} {:?}: a mapped token {v} was ignored", r.fixture_id)
                }
            }
        }
    }
    eprintln!("blink xml-roles: {trusted} trusted, {overridden} overridden by native semantics");
    assert!(trusted >= 100, "only {trusted} Blink tokens on the corpus");
}
