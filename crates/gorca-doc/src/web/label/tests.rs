// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::unnecessary_wraps
)]

use std::sync::Arc;

use super::*;
use crate::web::{DocOp, NodeSpec, SubtreeSpec};

const BR: NodeFlags = NodeFlags::BREAK_BEFORE;

fn node(role: Role, text: Option<&str>, flags: NodeFlags) -> SubtreeSpec {
    let mut n = NodeSpec::new(role);
    n.text = text.map(Arc::from);
    n.flags = flags;
    SubtreeSpec::leaf(n)
}

fn text(t: &str, flags: NodeFlags) -> SubtreeSpec {
    node(Role::StaticText, Some(t), flags)
}

fn ctl(role: Role, flags: NodeFlags) -> SubtreeSpec {
    node(role, Some("\u{FFFC}"), flags)
}

/// A document of `children` under a document root; returns it with the created ids in preorder
/// (root first).
fn doc(children: Vec<SubtreeSpec>) -> (WebDoc, Vec<DocNodeId>) {
    let mut d = WebDoc::new(1);
    let root = SubtreeSpec::with_children(NodeSpec::new(Role::DocumentWeb), children);
    let out = d
        .apply(&DocOp::InsertSubtree {
            parent: None,
            before: None,
            subtree: Arc::new(root),
        })
        .unwrap();
    (d, out.created)
}

fn r(x: i32, y: i32, w: u32, h: u32) -> Option<Rect> {
    Some(Rect { x, y, w, h })
}

#[test]
fn a_label_on_the_same_line_is_found_on_the_left() {
    let (d, ids) = doc(vec![
        text("Phone", BR),
        text(" ", NodeFlags::NONE),
        ctl(Role::TextEntry, NodeFlags::NONE),
    ]);
    let p = plan(&d, ids[3], Role::TextEntry).unwrap();
    assert_eq!(p.cands[0].side, Side::Left);
    assert_eq!(p.cands[0].text, "Phone");
    assert_eq!(p.probes()[1], ids[1]);
    // The label's box ends 12 px before the control, on its row.
    let rects = [r(100, 10, 120, 24), r(40, 12, 48, 18)];
    assert_eq!(choose(&p, &rects).as_deref(), Some("Phone"));
    // 300 px away: neither the near nor the wide rule takes it.
    let far = [r(400, 10, 120, 24), r(40, 12, 48, 18)];
    assert_eq!(choose(&p, &far), None);
    // 150 px away: only the wide (200 px) fallback does.
    let mid = [r(238, 10, 120, 24), r(40, 12, 48, 18)];
    assert_eq!(choose(&p, &mid).as_deref(), Some("Phone"));
}

#[test]
fn the_text_on_the_line_above_labels_a_control_that_starts_its_line() {
    let (d, ids) = doc(vec![
        text("Romeo unassociated sibling", BR),
        ctl(Role::TextEntry, BR),
    ]);
    let p = plan(&d, ids[2], Role::TextEntry).unwrap();
    let above = p.cands.iter().find(|c| c.side == Side::Above).unwrap();
    assert_eq!(above.text, "Romeo unassociated sibling");
    // 4 px below the text, not to the right of the control.
    let near = [r(10, 40, 200, 24), r(10, 18, 180, 18)];
    assert_eq!(
        choose(&p, &near).as_deref(),
        Some("Romeo unassociated sibling")
    );
    // Line boxes that overlap by 1 px are still adjacent (Chromium).
    let touching = [r(10, 40, 200, 24), r(10, 24, 180, 17)];
    assert!(choose(&p, &touching).is_some());
    // A paragraph 60 px higher is not its label.
    let far = [r(10, 120, 200, 24), r(10, 18, 180, 18)];
    assert_eq!(choose(&p, &far), None);
    // Text that starts to the right of the control is not a label above it.
    let right = [r(10, 40, 200, 24), r(300, 18, 180, 18)];
    assert_eq!(choose(&p, &right), None);
}

#[test]
fn a_widget_on_the_line_ends_the_left_text() {
    let (d, ids) = doc(vec![
        text("Name", BR),
        ctl(Role::Button, NodeFlags::NONE),
        text("Email", NodeFlags::NONE),
        ctl(Role::TextEntry, NodeFlags::NONE),
    ]);
    let p = plan(&d, ids[4], Role::TextEntry).unwrap();
    assert_eq!(p.cands[0].text, "Email");
}

#[test]
fn a_check_box_prefers_the_text_on_its_right() {
    let (d, ids) = doc(vec![
        text("Newsletter", BR),
        ctl(Role::CheckBox, NodeFlags::NONE),
        text("Subscribe to updates", NodeFlags::NONE),
    ]);
    let p = plan(&d, ids[2], Role::CheckBox).unwrap();
    let sides: Vec<Side> = p.cands.iter().map(|c| c.side).collect();
    assert_eq!(sides, [Side::Left, Side::Right]);
    // Left is 6 px away, right 8 px: the right text wins for a check box.
    let rects = [r(100, 10, 16, 16), r(30, 10, 64, 16), r(124, 10, 150, 16)];
    assert_eq!(choose(&p, &rects).as_deref(), Some("Subscribe to updates"));
    // A text entry keeps the left one.
    let q = plan(&d, ids[2], Role::TextEntry).unwrap();
    assert_eq!(choose(&q, &rects).as_deref(), Some("Newsletter"));
}

#[test]
fn a_combo_box_does_not_take_a_label_from_its_right() {
    let (d, ids) = doc(vec![
        ctl(Role::ComboBox, BR),
        text("Country", NodeFlags::NONE),
    ]);
    let p = plan(&d, ids[1], Role::ComboBox);
    assert!(p.is_none_or(|p| p.cands.iter().all(|c| c.side != Side::Right)));
}

#[test]
fn headings_and_list_items_are_never_labels() {
    let (d, ids) = doc(vec![
        SubtreeSpec::with_children(NodeSpec::new(Role::Heading), vec![text("Sign in", BR)]),
        ctl(Role::TextEntry, BR),
    ]);
    let p = plan(&d, ids[3], Role::TextEntry);
    assert!(p.is_none(), "{p:?}");
}

#[test]
fn only_a_text_entry_takes_a_label_from_below() {
    let (d, ids) = doc(vec![ctl(Role::TextEntry, BR), text("Email address", BR)]);
    let p = plan(&d, ids[1], Role::TextEntry).unwrap();
    assert!(p.cands.iter().any(|c| c.side == Side::Below));
    let rects = [r(10, 10, 200, 24), r(10, 40, 100, 18)];
    assert_eq!(choose(&p, &rects).as_deref(), Some("Email address"));
    let q = plan(&d, ids[1], Role::ComboBox);
    assert!(q.is_none_or(|q| q.cands.iter().all(|c| c.side != Side::Below)));
}

#[test]
fn a_control_without_a_rectangle_gets_nothing() {
    let (d, ids) = doc(vec![
        text("Phone", BR),
        ctl(Role::TextEntry, NodeFlags::NONE),
    ]);
    let p = plan(&d, ids[2], Role::TextEntry).unwrap();
    assert_eq!(choose(&p, &[None, r(0, 0, 40, 18)]), None);
    assert_eq!(choose(&p, &[r(50, 0, 100, 20), None]), None);
    // A zero-size box (display:none, collapsed) is no box.
    assert_eq!(choose(&p, &[r(50, 0, 0, 0), r(0, 0, 40, 18)]), None);
}

#[test]
fn label_text_is_cleaned_and_capped() {
    assert_eq!(
        clean("  Phone \u{FFFC}  number\n").as_deref(),
        Some("Phone number")
    );
    assert_eq!(clean(" \u{FFFC} "), None);
    assert_eq!(
        clean(&"x".repeat(500)).map(|s| s.chars().count()),
        Some(160)
    );
}

#[test]
fn the_role_gate_is_orcas() {
    for r in [
        Role::CheckBox,
        Role::ComboBox,
        Role::TextEntry,
        Role::PasswordEntry,
        Role::RadioButton,
    ] {
        assert!(infers_for(r), "{r:?}");
    }
    for r in [
        Role::Button,
        Role::Link,
        Role::Slider,
        Role::Panel,
        Role::SpinButton,
    ] {
        assert!(!infers_for(r), "{r:?}");
    }
}
