// SPDX-License-Identifier: GPL-3.0-or-later
//! WASM guest: wraps quirk-logic behind the WIT world.
// Generated export shims take one argument per flattened record field.
#![allow(clippy::too_many_arguments)]
wit_bindgen::generate!({ world: "quirk", path: "../wit" });

use quirk_logic as q;

struct G;

fn to_q(d: Delta) -> q::Delta {
    q::Delta {
        kind: match d.kind {
            DeltaKind::ChildrenAdded => q::Kind::ChildrenAdded,
            DeltaKind::ChildrenRemoved => q::Kind::ChildrenRemoved,
            DeltaKind::TextInserted => q::Kind::TextInserted,
            DeltaKind::TextRemoved => q::Kind::TextRemoved,
            DeltaKind::CaretMoved => q::Kind::CaretMoved,
            DeltaKind::AttrsChanged => q::Kind::AttrsChanged,
            DeltaKind::StateChanged => q::Kind::StateChanged,
            DeltaKind::Reorder => q::Kind::Reorder,
        },
        node: d.node,
        parent: d.parent,
        a: d.a,
        b: d.b,
        flags: d.flags,
        text: d.text,
    }
}

fn from_q(d: q::Delta) -> Delta {
    Delta {
        kind: match d.kind {
            q::Kind::ChildrenAdded => DeltaKind::ChildrenAdded,
            q::Kind::ChildrenRemoved => DeltaKind::ChildrenRemoved,
            q::Kind::TextInserted => DeltaKind::TextInserted,
            q::Kind::TextRemoved => DeltaKind::TextRemoved,
            q::Kind::CaretMoved => DeltaKind::CaretMoved,
            q::Kind::AttrsChanged => DeltaKind::AttrsChanged,
            q::Kind::StateChanged => DeltaKind::StateChanged,
            q::Kind::Reorder => DeltaKind::Reorder,
        },
        node: d.node,
        parent: d.parent,
        a: d.a,
        b: d.b,
        flags: d.flags,
        text: d.text,
    }
}

impl Guest for G {
    fn noop() {}

    fn walk(start: (u64, u64), k: u32) -> u32 {
        let mut hits = 0;
        let mut key = start;
        for _ in 0..k {
            match get_node(key) {
                Some(n) => {
                    if q::on_key(0x41, n.role, n.states).is_some() {
                        hits += 1;
                    }
                    key = (n.key.0, n.key.1 + 1);
                }
                None => break,
            }
        }
        hits
    }

    fn normalize(engine: u8, ds: Vec<Delta>) -> Vec<Delta> {
        let qs: Vec<q::Delta> = ds.into_iter().map(to_q).collect();
        q::normalize(engine, qs).into_iter().map(from_q).collect()
    }

    fn classify(engine: u8, ds: Vec<Delta>) -> Vec<u8> {
        let mut out = Vec::with_capacity(ds.len());
        for d in ds {
            let mut c = to_q(d);
            out.push(q::fix(engine, &mut c));
        }
        out
    }

    fn on_key(key: u32, n: Node) -> Option<bool> {
        q::on_key(key, n.role, n.states)
    }

    fn present(n: Node) -> Option<Vec<Segment>> {
        let name = n.name?;
        Some(vec![
            Segment {
                text: name,
                kind: SegKind::Text,
            },
            Segment {
                text: String::from("heading level 2"),
                kind: SegKind::Role,
            },
        ])
    }

    fn trap() {
        #[cfg(target_arch = "wasm32")]
        core::arch::wasm32::unreachable();
    }

    fn spin() {
        let mut x: u64 = 0;
        loop {
            x = core::hint::black_box(x.wrapping_add(1));
        }
    }
}

export!(G);
