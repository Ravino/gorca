// SPDX-License-Identifier: GPL-3.0-or-later
//! Gorca-side cost of the review hot paths (M22b3, M-6 for review): an object move of simple
//! review (OB1), the validity hash a step checks for every node of the model (VD1), and a whole
//! line-model build (FL1). They measure what the Reader does with the mirror between receiving
//! the key and handing the words over; the end-to-end figures come from the e2e run
//! (`tests/shell/run_p3.sh`).
//!
//! The same test runs in the legacy tree (the tag `legacy/review-last`) and in this one:
//! `bench/m6/review-latency.sh` alternates them on one host. Run it by hand with
//! `cargo test --release -p gorca-core --lib review::latency -- --ignored --nocapture`.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::unreadable_literal,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gorca_model::{Lookup, NodeKey, NodeSnapshot, NodeView, PropMask, Rect, Role, StateSet};

use super::cluster::{BuildInput, BuildNode, build_line_model};
use super::collect::testsrc::{key, snap};
use super::model::ReviewLimits;
use super::objnav::{NavMove, NavSource, StepResult, step};
use super::validity::snapshot_hash;

struct Page {
    views: HashMap<NodeKey, NodeView>,
    /// Pre-order, with depth.
    order: Vec<(NodeKey, u16)>,
}

impl NavSource for Page {
    fn view(&self, k: NodeKey) -> Lookup<NodeView> {
        self.views
            .get(&k)
            .cloned()
            .map_or(Lookup::Gone, Lookup::Hit)
    }
}

/// A window of `rows` rows, each a panel holding a label, a text entry, a button and a check box:
/// 5 nodes a row. Every node carries the entry a live mirror holds (the provisional one of its
/// source record) and its extents.
fn page(rows: usize) -> Page {
    let mut snaps: Vec<NodeSnapshot> = Vec::new();
    let mut next = 2u64;
    let mut row_ids = Vec::new();
    for i in 0..rows {
        let (panel, label, entry, button, check) = (next, next + 1, next + 2, next + 3, next + 4);
        next += 5;
        row_ids.push(panel);
        let y = 10 + i32::try_from(i).unwrap_or(0) * 24;
        let node = |id, role, name: &str, parent, kids: &[u64], x: i32, w: u32| {
            let mut s = snap(id, role, name, Some(parent), kids);
            s.bounds = Some(Rect { x, y, w, h: 20 });
            s.have |= PropMask::BOUNDS;
            s
        };
        snaps.push(node(
            panel,
            Role::Panel,
            "",
            1,
            &[label, entry, button, check],
            0,
            700,
        ));
        snaps.push(node(
            label,
            Role::Label,
            &format!("Field {i}"),
            panel,
            &[],
            0,
            100,
        ));
        let mut e = node(
            entry,
            Role::TextEntry,
            &format!("Entry {i}"),
            panel,
            &[],
            110,
            200,
        );
        e.states |= StateSet::FOCUSABLE | StateSet::EDITABLE;
        snaps.push(e);
        let mut b = node(
            button,
            Role::Button,
            &format!("Apply {i}"),
            panel,
            &[],
            320,
            80,
        );
        b.states |= StateSet::FOCUSABLE;
        snaps.push(b);
        let mut c = node(
            check,
            Role::CheckBox,
            &format!("Option {i}"),
            panel,
            &[],
            410,
            90,
        );
        c.states |= StateSet::FOCUSABLE;
        snaps.push(c);
    }
    let mut root = snap(1, Role::Window, "Window", None, &row_ids);
    root.bounds = Some(Rect {
        x: 0,
        y: 0,
        w: 800,
        h: 10_000,
    });
    root.have |= PropMask::BOUNDS;
    let mut all = vec![root];
    all.extend(snaps);
    let mut views = HashMap::new();
    let mut order = Vec::new();
    for s in all {
        let mut s = s;
        // The entry the app actor writes into the mirror next to the record.
        if s.sem.entry.is_none()
            && let Some(r) = s.sem.rec.as_ref()
        {
            s.sem.entry = Some(Arc::new(r.provisional_entry()));
        }
        let depth = match s.parent {
            None => 0,
            Some(p) if p == key(1) => 1,
            Some(_) => 2,
        };
        order.push((s.key, depth));
        views.insert(
            s.key,
            NodeView {
                snap: Arc::new(s),
                stale: PropMask::empty(),
                aged: false,
            },
        );
    }
    Page { views, order }
}

struct Xor(u64);

impl Xor {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

fn pct(mut v: Vec<Duration>, p: f64) -> Duration {
    v.sort();
    v[((v.len() as f64 * p) as usize).min(v.len() - 1)]
}

fn line(nodes: usize, name: &str, v: Vec<Duration>) {
    eprintln!(
        "{nodes} nodes: {name}: p50 {:?} p99 {:?}",
        pct(v.clone(), 0.5),
        pct(v, 0.99)
    );
}

const MOVES: [(&str, NavMove); 5] = [
    ("OB1 object next", NavMove::Next),
    ("OB1 object previous", NavMove::Prev),
    ("OB1 object parent", NavMove::Parent),
    ("OB1 object first child", NavMove::FirstChild),
    ("OB1 object next in flow", NavMove::NextInFlow),
];

fn series(rows: usize) {
    let p = page(rows);
    let n = p.views.len();
    let limits = ReviewLimits::default();
    let keys: Vec<NodeKey> = p.order.iter().map(|(k, _)| *k).collect();
    let mut rng = Xor(0x9E37_79B9_7F4A_7C15);
    for (name, mv) in MOVES {
        let mut v = Vec::with_capacity(20_000);
        for _ in 0..20_000 {
            let k = keys[(rng.next() % keys.len() as u64) as usize];
            let t = Instant::now();
            let r = step(&p, k, mv, true, limits);
            v.push(t.elapsed());
            assert!(
                !matches!(r, StepResult::Stop(_)),
                "{name}: a stop on a full page"
            );
        }
        line(n, name, v);
    }
    // VD1: the validity hash of a node, in batches of 100.
    let snaps: Vec<Arc<NodeSnapshot>> = p.views.values().map(|v| Arc::clone(&v.snap)).collect();
    let mut v = Vec::new();
    let mut sink = 0u64;
    for _ in 0..30 {
        for chunk in snaps.chunks(100) {
            let t = Instant::now();
            for s in chunk {
                sink ^= snapshot_hash(s);
            }
            v.push(t.elapsed() / chunk.len() as u32);
        }
    }
    assert_ne!(sink, 1);
    line(n, "VD1 validity hash per node", v);
    // FL1: one whole line-model build.
    let nodes: Vec<BuildNode> = p
        .order
        .iter()
        .map(|(k, depth)| BuildNode::of(Arc::clone(&p.views[k].snap), *depth))
        .collect();
    let input = BuildInput {
        app: key(1).app,
        surface: key(1),
        nodes,
        built_rev: 1,
        built_at: Instant::now(),
        truncated: None,
    };
    let mut v = Vec::new();
    for _ in 0..40 {
        let t = Instant::now();
        let m = build_line_model(&input, &limits);
        v.push(t.elapsed());
        assert!(!m.lines.is_empty());
    }
    line(n, "FL1 line-model build", v);
}

#[test]
#[ignore = "release-mode measurement; see bench/m6/review-latency.sh"]
fn gates_ob1_vd1_and_fl1_on_1k_and_10k_pages() {
    series(200);
    series(2_000);
}

#[test]
fn the_series_run_in_a_debug_build() {
    // A small page, only to keep the measurement code compiling and sound under `cargo test`.
    let p = page(8);
    let r = step(&p, key(3), NavMove::Next, true, ReviewLimits::default());
    assert!(matches!(r, StepResult::Found(_) | StepResult::Empty));
}

/// Pre-order of the subtree at `k`, with depths.
fn walk(
    k: NodeKey,
    depth: u16,
    by_id: &HashMap<NodeKey, NodeSnapshot>,
    out: &mut Vec<(NodeKey, u16)>,
) {
    out.push((k, depth));
    if let gorca_model::ChildList::Known(kids) = &by_id[&k].children {
        for c in kids {
            walk(*c, depth + 1, by_id, out);
        }
    }
}

/// Rows of mixed roles: a label before an entry, a control that holds a label with its own name,
/// links, list items, check boxes, tabs and menu items. The shapes the line-building rules
/// (control zones, named containers, label-like text) decide on.
fn mixed_page() -> Page {
    type Spec<'a> = (Role, &'a str, Vec<(Role, &'a str)>);
    let rows: Vec<Vec<Spec<'_>>> = vec![
        vec![
            (Role::Label, "Name", vec![]),
            (Role::TextEntry, "Name", vec![]),
            (Role::Button, "Go", vec![(Role::Label, "Go")]),
        ],
        vec![
            (Role::StaticText, "Hello", vec![]),
            (Role::Link, "Docs", vec![(Role::StaticText, "Docs")]),
        ],
        vec![(Role::ListItem, "One", vec![(Role::StaticText, "One")])],
        vec![
            (Role::CheckBox, "Opt", vec![]),
            (Role::RadioButton, "Radio", vec![(Role::Label, "Radio")]),
            (Role::ToggleButton, "Tog", vec![]),
        ],
        vec![
            (Role::Tab, "Tab", vec![(Role::Label, "Tab")]),
            (Role::MenuItem, "File", vec![(Role::Label, "File")]),
        ],
        vec![
            (Role::Panel, "Group", vec![(Role::StaticText, "inside")]),
            (Role::Heading, "Title", vec![(Role::StaticText, "Title")]),
        ],
        vec![
            (Role::Cell, "cell", vec![(Role::StaticText, "cell")]),
            (Role::Slider, "Vol", vec![]),
            (Role::ComboBox, "Pick", vec![]),
        ],
    ];
    let mut snaps: Vec<NodeSnapshot> = Vec::new();
    let mut next = 2u64;
    let mut row_ids = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        let y = 10 + i32::try_from(i).unwrap_or(0) * 24;
        let panel = next;
        next += 1;
        row_ids.push(panel);
        let mut kid_ids = Vec::new();
        let mut col = 0;
        let mut built: Vec<NodeSnapshot> = Vec::new();
        for (role, name, kids) in row {
            let id = next;
            next += 1;
            kid_ids.push(id);
            let mut sub = Vec::new();
            for (krole, kname) in kids {
                let kid = next;
                next += 1;
                sub.push(kid);
                let mut k = snap(kid, *krole, kname, Some(id), &[]);
                k.bounds = Some(Rect {
                    x: col + 2,
                    y,
                    w: 60,
                    h: 20,
                });
                k.have |= PropMask::BOUNDS;
                built.push(k);
            }
            let mut n = snap(id, *role, name, Some(panel), &sub);
            n.bounds = Some(Rect {
                x: col,
                y,
                w: 100,
                h: 20,
            });
            n.have |= PropMask::BOUNDS;
            n.states |= StateSet::FOCUSABLE;
            snaps.push(n);
            snaps.append(&mut built);
            col += 110;
        }
        let mut pn = snap(panel, Role::Panel, "", Some(1), &kid_ids);
        pn.bounds = Some(Rect {
            x: 0,
            y,
            w: 700,
            h: 20,
        });
        pn.have |= PropMask::BOUNDS;
        snaps.push(pn);
    }
    let mut root = snap(1, Role::Window, "Window", None, &row_ids);
    root.bounds = Some(Rect {
        x: 0,
        y: 0,
        w: 800,
        h: 400,
    });
    root.have |= PropMask::BOUNDS;
    snaps.insert(0, root);
    // Pre-order: a node, then its subtree.
    let by_id: HashMap<NodeKey, NodeSnapshot> = snaps.into_iter().map(|s| (s.key, s)).collect();
    let mut order = Vec::new();
    walk(key(1), 0, &by_id, &mut order);
    let mut views = HashMap::new();
    for (k, s) in by_id {
        let mut s = s;
        if s.sem.entry.is_none()
            && let Some(r) = s.sem.rec.as_ref()
        {
            s.sem.entry = Some(Arc::new(r.provisional_entry()));
        }
        views.insert(
            k,
            NodeView {
                snap: Arc::new(s),
                stale: PropMask::empty(),
                aged: false,
            },
        );
    }
    Page { views, order }
}

/// The flat-review line texts of the mixed page, one per line of the file named by
/// `GORCA_LINES_OUT` (or stderr): the same test runs in the legacy tree and in this one, and the
/// two files are diffed (`bench/m6/review-lines.sh`).
#[test]
#[ignore = "dumps the line model for the A/B diff of the cut-over; see bench/m6/review-lines.sh"]
fn dump_line_texts_of_the_mixed_page() {
    let p = mixed_page();
    let nodes: Vec<BuildNode> = p
        .order
        .iter()
        .map(|(k, depth)| BuildNode::of(Arc::clone(&p.views[k].snap), *depth))
        .collect();
    let input = BuildInput {
        app: key(1).app,
        surface: key(1),
        nodes,
        built_rev: 1,
        built_at: Instant::now(),
        truncated: None,
    };
    let m = build_line_model(&input, &ReviewLimits::default());
    let mut text = String::new();
    for l in &m.lines {
        text.push_str(&l.text);
        text.push('\n');
    }
    for z in &m.zones {
        let _ = writeln!(text, "zone {:?} {}", z.kind, z.text);
    }
    assert!(!m.lines.is_empty());
    match std::env::var("GORCA_LINES_OUT") {
        Ok(path) => std::fs::write(path, text).expect("write"),
        Err(_) => eprintln!("{text}"),
    }
}
