// SPDX-License-Identifier: GPL-3.0-or-later
//! The one way browse mode reads the text of a document range for speech, and the one question
//! "is this node a secret field" for document nodes. The text under a secret field (a toolkit
//! password entry, whatever role the fusion settled on) is never spoken, by character, word, line,
//! say-all, selection or find: [`text_in`] masks it. Rule 12 (`raw-text-in`) bans `.text_in(` in
//! the speech paths of `browse/` and `doc_actor/live.rs`, so a new path cannot skip it.

use gorca_doc::web::{NodeView, WebDoc};
use gorca_model::DocNodeId;

/// What a secret character is read as: a bullet, in place, so offsets stay the document's.
const MASK: char = '\u{2022}';

/// Whether a document node is a secret field: its fused entry says so (the role or the source's
/// mark), or, without an entry, its role. Every sink of browse mode asks this.
pub(crate) fn node_is_secret(v: &NodeView<'_>) -> bool {
    v.sem().as_deref().map_or_else(
        || gorca_present::is_secret_role(v.role),
        gorca_present::is_secret_entry,
    )
}

/// Whether `id` or an ancestor is a secret field (its text is never read, mirrored or copied).
pub(crate) fn secret(d: &WebDoc, id: DocNodeId) -> bool {
    let mut cur = Some(id);
    for _ in 0..64 {
        let Some(n) = cur else { return false };
        if d.node(n).is_some_and(|v| node_is_secret(&v)) {
            return true;
        }
        cur = d.parent(n);
    }
    false
}

/// The text of the flat range `[a, b)` as it may be spoken: the text of a secret node, or of a node
/// under one, is replaced by bullets of the same length.
pub(crate) fn text_in(d: &WebDoc, a: u32, b: u32) -> String {
    let t = d.text_in(a, b);
    if t.is_empty() {
        return t;
    }
    let Some((n0, local0)) = d.at_offset(a) else {
        return t;
    };
    let mut masked: Option<Vec<char>> = None;
    let mut off = a - local0.min(a);
    for id in d.preorder_from(n0) {
        if off >= b {
            break;
        }
        let own = d.own_len(id).unwrap_or(0);
        let (lo, hi) = (a.max(off), b.min(off + own));
        if lo < hi && secret(d, id) {
            let cs = masked.get_or_insert_with(|| t.chars().collect());
            for i in lo..hi {
                if let Some(c) = cs.get_mut((i - a) as usize) {
                    *c = MASK;
                }
            }
        }
        off += own;
    }
    masked.map_or(t, |cs| cs.into_iter().collect())
}
