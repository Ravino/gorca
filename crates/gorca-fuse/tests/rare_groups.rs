// SPDX-License-Identifier: GPL-3.0-or-later
//! The rare-text and name-source groups (M22a-C3, review W-6): `roledescription`, `placeholder`,
//! `keyshortcuts`, `valuetext` and `namefrom` are registers like any other, so the fused entry
//! carries them and a speech reader never has to go back to the source record.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use gorca_fuse::{
    Budget, Input, Ivm, MAX_TEXT_BYTES, NoPatches, NoQuirks, NodeIdx, PrimarySlot, Profile, Rules,
    Val, Write, rare_write,
};
use gorca_model::sem::{Group, NameFrom, ProvCode, SemRole, SemText, Semantics};

const P0: PrimarySlot = PrimarySlot::P0;
const RARE: [Group; 5] = [
    Group::RoleDescription,
    Group::Placeholder,
    Group::KeyShortcuts,
    Group::ValueText,
    Group::NameFrom,
];

fn engine() -> Ivm<NoQuirks, NoPatches> {
    let profile = Profile::new(&[P0], Rules::aria());
    let mut e = Ivm::new(profile, NoQuirks, NoPatches);
    e.add_node(NodeIdx(0), None).unwrap();
    e
}

fn text(f: impl FnOnce(&mut SemText)) -> Semantics {
    let mut t = SemText::default();
    f(&mut t);
    Semantics {
        role: SemRole::TextBox,
        name_from: NameFrom::Placeholder,
        text: Some(Arc::new(t)),
        ..Semantics::default()
    }
}

fn feed(e: &mut Ivm<NoQuirks, NoPatches>, seq: u64, sem: &Semantics) {
    let mut inputs = vec![Input::Life {
        node: NodeIdx(0),
        src: P0,
        seq,
        alive: true,
    }];
    for g in RARE {
        inputs.push(Input::Obs {
            node: NodeIdx(0),
            src: P0,
            seq,
            write: rare_write(sem, g).unwrap(),
        });
    }
    e.apply(inputs);
    e.run(Budget::UNLIMITED);
}

#[test]
fn the_entry_carries_every_rare_field_with_primary_provenance() {
    let mut e = engine();
    let sem = text(|t| {
        t.role_description = Some(Arc::from("slide"));
        t.placeholder = Some(Arc::from("Search"));
        t.key_shortcuts = Some(Arc::from("Control+K"));
        t.value_text = Some(Arc::from("3 of 10"));
    });
    feed(&mut e, 1, &sem);
    let v = e.entry_view(NodeIdx(0)).unwrap();
    let t = v.sem.text.as_deref().unwrap();
    assert_eq!(t.role_description.as_deref(), Some("slide"));
    assert_eq!(t.placeholder.as_deref(), Some("Search"));
    assert_eq!(t.key_shortcuts.as_deref(), Some("Control+K"));
    assert_eq!(t.value_text.as_deref(), Some("3 of 10"));
    assert_eq!(v.sem.name_from, NameFrom::Placeholder);
    for g in RARE {
        assert!(v.known.has(g), "{g:?} known");
        assert_eq!(v.prov.get(g), Some(ProvCode::ObservedPrimary), "{g:?}");
    }
    // The description is a different register and stays unset.
    assert_eq!(v.sem.description(), None);
    assert!(!v.known.has(Group::Description));
    assert!(e.verify().is_ok());
}

#[test]
fn an_absent_or_empty_field_clears_and_an_older_write_cannot_bring_it_back() {
    let mut e = engine();
    let full = text(|t| {
        t.placeholder = Some(Arc::from("Search"));
        t.value_text = Some(Arc::from("high"));
    });
    feed(&mut e, 5, &full);
    // A newer observation with the fields gone: Absent wins over the older values.
    let empty = Semantics {
        role: SemRole::TextBox,
        ..Semantics::default()
    };
    feed(&mut e, 6, &empty);
    let v = e.entry_view(NodeIdx(0)).unwrap();
    assert_eq!(v.sem.text, None, "an empty SemText is dropped");
    assert_eq!(v.sem.name_from, NameFrom::None);
    assert!(v.known.has(Group::Placeholder), "Absent is known");
    // A late reply with the old values changes nothing.
    feed(&mut e, 4, &full);
    let v = e.entry_view(NodeIdx(0)).unwrap();
    assert_eq!(v.sem.text, None);
}

#[test]
fn the_registers_join_to_the_same_entry_in_every_delivery_order() {
    let a = text(|t| t.placeholder = Some(Arc::from("one")));
    let b = text(|t| t.placeholder = Some(Arc::from("two")));
    let mut x = engine();
    feed(&mut x, 1, &a);
    feed(&mut x, 2, &b);
    let mut y = engine();
    feed(&mut y, 2, &b);
    feed(&mut y, 1, &a);
    let mut z = engine();
    feed(&mut z, 2, &b);
    feed(&mut z, 1, &a);
    feed(&mut z, 2, &b);
    // `sem_rev` counts publications, so it depends on the history, not on the registers.
    let view = |e: &Ivm<NoQuirks, NoPatches>| {
        let mut v = (*e.entry_view(NodeIdx(0)).unwrap()).clone();
        v.sem_rev = 0;
        v
    };
    let ex = view(&x);
    assert_eq!(ex, view(&y));
    assert_eq!(ex, view(&z));
    assert_eq!(
        ex.sem.text.as_deref().unwrap().placeholder.as_deref(),
        Some("two")
    );
}

#[test]
fn a_hostile_long_string_is_cut_at_the_register() {
    let mut e = engine();
    let long = "é".repeat(MAX_TEXT_BYTES);
    let sem = text(|t| t.key_shortcuts = Some(Arc::from(long.as_str())));
    feed(&mut e, 1, &sem);
    let v = e.entry_view(NodeIdx(0)).unwrap();
    let got = v
        .sem
        .text
        .as_deref()
        .unwrap()
        .key_shortcuts
        .as_deref()
        .unwrap();
    assert!(got.len() <= MAX_TEXT_BYTES);
    assert!(got.chars().all(|c| c == 'é'), "cut on a char boundary");
}

#[test]
fn rare_write_only_answers_for_its_groups() {
    let sem = text(|t| t.placeholder = Some(Arc::from("p")));
    for g in Group::ALL {
        assert_eq!(rare_write(&sem, *g).is_some(), g.is_primary_only(), "{g:?}");
    }
    assert_eq!(
        rare_write(&sem, Group::Placeholder),
        Some(Write::Val(Val::Placeholder(Arc::from("p"))))
    );
    assert_eq!(
        rare_write(&sem, Group::ValueText),
        Some(Write::Absent(Group::ValueText))
    );
}
