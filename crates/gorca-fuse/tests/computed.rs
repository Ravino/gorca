// SPDX-License-Identifier: GPL-3.0-or-later
//! Computed S1 patches (ADR-0026 section 2.6, PF-3, PF-5, PF-6): a plugin's `facts.assert(layer =
//! patch)` overrides an observed value at S1, never touches S0, carries its taint, ranks by
//! `(taint, source name, group)` and leaves the model when the source is cleared.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use gorca_fuse::{
    Budget, Input, Ivm, NoPatches, NoQuirks, NodeIdx, PrimarySlot, Profile, Rules, Val, Write,
};
use gorca_model::sem::{Group, ProvCode, SemRole, TrustTier};

const P0: PrimarySlot = PrimarySlot::P0;

fn engine() -> Ivm<NoQuirks, NoPatches> {
    let mut e = Ivm::new(Profile::new(&[P0], Rules::aria()), NoQuirks, NoPatches);
    e.add_node(NodeIdx(0), None).unwrap();
    e.apply([
        Input::Life {
            node: NodeIdx(0),
            src: P0,
            seq: 1,
            alive: true,
        },
        Input::Obs {
            node: NodeIdx(0),
            src: P0,
            seq: 1,
            write: Write::Val(Val::Role(SemRole::Button)),
        },
        Input::Obs {
            node: NodeIdx(0),
            src: P0,
            seq: 1,
            write: Write::Val(Val::Name(Arc::from("AL Alice 15:00"))),
        },
    ]);
    e.run(Budget::UNLIMITED);
    e
}

fn name(e: &Ivm<NoQuirks, NoPatches>) -> Option<String> {
    e.entry_view(NodeIdx(0))
        .unwrap()
        .name
        .as_deref()
        .map(str::to_owned)
}

fn nm(s: &str) -> Write {
    Write::Val(Val::Name(Arc::from(s)))
}

#[test]
fn a_computed_patch_overrides_the_observed_name_and_leaves_with_its_source() {
    let mut e = engine();
    assert_eq!(name(&e).as_deref(), Some("AL Alice 15:00"));
    let src: Arc<str> = Arc::from("gorca.demo");
    assert!(e.set_computed(
        NodeIdx(0),
        &src,
        nm("Alice, 1 unread"),
        TrustTier::T2FirstParty
    ));
    e.run(Budget::UNLIMITED);
    assert_eq!(name(&e).as_deref(), Some("Alice, 1 unread"));
    let entry = e.entry_view(NodeIdx(0)).unwrap();
    assert_eq!(entry.prov.get(Group::Name), Some(ProvCode::Patch));
    // Setting the same value again changes nothing.
    assert!(!e.set_computed(
        NodeIdx(0),
        &src,
        nm("Alice, 1 unread"),
        TrustTier::T2FirstParty
    ));
    e.verify().expect("incremental equals from scratch");
    // The source is cleared (quarantine / unload): the observed value is back.
    assert_eq!(e.clear_computed_source("gorca.demo"), vec![NodeIdx(0)]);
    e.run(Budget::UNLIMITED);
    assert_eq!(name(&e).as_deref(), Some("AL Alice 15:00"));
    assert_eq!(e.computed_len(), 0);
}

#[test]
fn ranking_is_total_and_does_not_depend_on_arrival_order() {
    let a: Arc<str> = Arc::from("a.plugin");
    let b: Arc<str> = Arc::from("b.plugin");
    let mut names = Vec::new();
    for order in [[0, 1], [1, 0]] {
        let mut e = engine();
        for i in order {
            if i == 0 {
                e.set_computed(NodeIdx(0), &a, nm("from a"), TrustTier::T2FirstParty);
            } else {
                e.set_computed(NodeIdx(0), &b, nm("from b"), TrustTier::T2FirstParty);
            }
        }
        e.run(Budget::UNLIMITED);
        names.push(name(&e));
    }
    assert_eq!(names[0], names[1]);
    assert_eq!(
        names[0].as_deref(),
        Some("from a"),
        "plugin id order breaks the tie"
    );
}

#[test]
fn first_party_outranks_a_trusted_third_party_and_taint_is_recorded() {
    let tp: Arc<str> = Arc::from("a.third");
    let fp: Arc<str> = Arc::from("z.first");
    let mut e = engine();
    e.set_computed(NodeIdx(0), &tp, nm("third"), TrustTier::T3Untrusted);
    e.run(Budget::UNLIMITED);
    assert_eq!(name(&e).as_deref(), Some("third"));
    assert_eq!(
        e.entry_view(NodeIdx(0)).unwrap().taint.get(Group::Name),
        TrustTier::T3Untrusted.code()
    );
    e.set_computed(NodeIdx(0), &fp, nm("first"), TrustTier::T2FirstParty);
    e.run(Budget::UNLIMITED);
    assert_eq!(name(&e).as_deref(), Some("first"));
    e.verify().expect("consistent");
}

#[test]
fn a_removed_node_drops_its_patches() {
    let mut e = engine();
    let src: Arc<str> = Arc::from("gorca.demo");
    e.set_computed(NodeIdx(0), &src, nm("x"), TrustTier::T2FirstParty);
    e.remove_subtree(NodeIdx(0)).unwrap();
    assert_eq!(e.computed_len(), 0);
}
