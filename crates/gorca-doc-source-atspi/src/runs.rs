//! TX-1 text runs (ADR-0003 AM-5, browse-mode design FT-1, FT-2).
//!
//! Firefox keeps the text of a container in the container's hypertext, with one U+FFFC for each
//! embedded child, and exposes no text leaves. The mirror wants one canonical shape: text leaves
//! as nodes. [`RunLayer`] sits between the fetch/identity layers and the sink and rewrites a
//! stream of deltas into that shape:
//!
//! * the `text` of every `Upsert` (and every `Text` replacement) is taken out of the node;
//! * the text is split at U+FFFC into *run* leaves (`Role::StaticText`, no transport key),
//!   interleaved with the real children: `[run0, child0, run1, child1, ...]`;
//! * a run's id is `RUN_TAG | mix(parent, run index)`, so an edit of one run is an in-place
//!   `Upsert` and an unchanged run is never sent twice;
//! * phantom U+FEFF is stripped (Firefox appends one after every astral scalar), so everything
//!   downstream counts Unicode scalars (TU-2).
//!
//! Empty runs are omitted. When the number of U+FFFC and of children disagree (Chromium, or a
//! page that changed between the two reads) the pairing is by position: surplus children follow
//! the last run, surplus separators are ignored. The layer is pure and tested without D-Bus.

use std::collections::HashMap;
use std::sync::Arc;

use gorca_model::{DocDelta, Role, SrcNode, SrcNodeId, TextEdit};
use smallvec::SmallVec;

use crate::roles::DEFAULT_PLAIN_STATES;

/// Tag bits of a run id: bit 62 and bit 61 set, bit 63 clear. Transport keys are small counters,
/// identity-synthesised ids carry bit 62 only, side-table keys carry bit 63.
pub const RUN_TAG: u64 = (1 << 62) | (1 << 61);
const HASH_MASK: u64 = (1 << 60) - 1;
const OBJECT: char = '\u{FFFC}';
const PHANTOM: char = '\u{FEFF}';

/// Id of run `index` of container `parent`.
pub fn run_id(parent: SrcNodeId, index: usize) -> SrcNodeId {
    let mut x = parent
        .0
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add((index as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F))
        .wrapping_add(0x1656_67B1_9E37_79F9);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^= x >> 31;
    SrcNodeId(RUN_TAG | (x & HASH_MASK))
}

/// True for ids made by [`run_id`].
pub fn is_run(id: SrcNodeId) -> bool {
    id.0 & (RUN_TAG | (1 << 63)) == RUN_TAG
}

/// Strip the phantom U+FEFF Firefox appends after astral scalars.
pub fn clean_text(t: &str) -> Arc<str> {
    if t.contains(PHANTOM) {
        t.chars()
            .filter(|c| *c != PHANTOM)
            .collect::<String>()
            .into()
    } else {
        Arc::from(t)
    }
}

#[derive(Debug, Default)]
struct Container {
    /// The text split at U+FFFC (`n + 1` parts for `n` objects). Empty when the node has no text.
    parts: Vec<Arc<str>>,
    /// The real children, in order, when a `Children` delta was seen.
    kids: Option<Vec<SrcNodeId>>,
    /// Runs currently in the mirror: `(id, text)` by part index.
    runs: Vec<(SrcNodeId, Arc<str>)>,
    /// The child list last sent downstream.
    sent: Option<Vec<SrcNodeId>>,
    dirty: bool,
}

/// Rewrites delta batches into the run-leaf shape.
#[derive(Debug, Default)]
pub struct RunLayer {
    containers: HashMap<SrcNodeId, Container>,
}

fn split_parts(text: &str) -> Vec<Arc<str>> {
    text.split(OBJECT).map(Arc::from).collect()
}

fn run_node(id: SrcNodeId, text: &Arc<str>) -> SrcNode {
    SrcNode {
        id,
        role: Role::StaticText,
        states: DEFAULT_PLAIN_STATES,
        name: None,
        text: Some(Arc::clone(text)),
        attrs: SmallVec::new(),
        local: None,
    }
}

impl RunLayer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of containers tracked (tests, metrics).
    pub fn tracked(&self) -> usize {
        self.containers.len()
    }

    /// Rewrite one batch. The output keeps the contract order inside the batch: `Reset`,
    /// `Upsert`s, `Children`, `Removed`, then `Settled` and hints; the doc actor applies a batch
    /// as a unit anyway.
    pub fn process(&mut self, deltas: Vec<DocDelta>) -> Vec<DocDelta> {
        let mut head: Vec<DocDelta> = Vec::new();
        let mut upserts: Vec<SrcNode> = Vec::new();
        let mut removed: Vec<SrcNodeId> = Vec::new();
        let mut tail: Vec<DocDelta> = Vec::new();
        for d in deltas {
            match d {
                DocDelta::Reset { root, nav_id } => {
                    self.containers.clear();
                    head.push(DocDelta::Reset { root, nav_id });
                }
                DocDelta::Upsert(mut nodes) => {
                    for n in &mut nodes {
                        let c = self.containers.entry(n.id).or_default();
                        c.parts = match n.text.take() {
                            Some(t) if !t.is_empty() => split_parts(&clean_text(&t)),
                            Some(_) | None => Vec::new(),
                        };
                        c.dirty = true;
                    }
                    upserts.extend(nodes);
                }
                DocDelta::Children { parent, children } => {
                    let c = self.containers.entry(parent).or_default();
                    c.kids = Some(children.to_vec());
                    c.dirty = true;
                }
                DocDelta::Text { node, edit } => match edit {
                    TextEdit::Replace(t) => {
                        let c = self.containers.entry(node).or_default();
                        c.parts = if t.is_empty() {
                            Vec::new()
                        } else {
                            split_parts(&clean_text(&t))
                        };
                        c.dirty = true;
                    }
                    TextEdit::Splice { .. } => head.push(DocDelta::Text { node, edit }),
                },
                DocDelta::Removed(ids) => {
                    for id in &ids {
                        // The runs leave with their container's subtree.
                        self.containers.remove(id);
                    }
                    removed.extend(ids);
                }
                d @ (DocDelta::Settled { .. } | DocDelta::Hint(_)) => tail.push(d),
            }
        }
        let mut lists: Vec<DocDelta> = Vec::new();
        let mut gone: Vec<SrcNodeId> = Vec::new();
        self.flush(&mut upserts, &mut lists, &mut gone);
        removed.extend(gone);
        let mut out = head;
        if !upserts.is_empty() {
            out.push(DocDelta::Upsert(upserts));
        }
        out.append(&mut lists);
        if !removed.is_empty() {
            out.push(DocDelta::Removed(removed));
        }
        out.append(&mut tail);
        out
    }

    /// Emit the runs and the combined child list of every dirty container.
    fn flush(
        &mut self,
        upserts: &mut Vec<SrcNode>,
        lists: &mut Vec<DocDelta>,
        gone: &mut Vec<SrcNodeId>,
    ) {
        for (parent, c) in &mut self.containers {
            if !c.dirty {
                continue;
            }
            c.dirty = false;
            let parts = &c.parts;
            let kids: &[SrcNodeId] = c.kids.as_deref().unwrap_or(&[]);
            let mut children: Vec<SrcNodeId> = Vec::with_capacity(parts.len() + kids.len());
            let mut runs: Vec<(SrcNodeId, Arc<str>)> = Vec::new();
            for (i, part) in parts.iter().enumerate() {
                if !part.is_empty() {
                    let id = run_id(*parent, i);
                    // Skip a run the mirror already has with the same text.
                    let same = c.runs.iter().any(|(rid, rt)| *rid == id && **rt == **part);
                    if !same {
                        upserts.push(run_node(id, part));
                    }
                    runs.push((id, Arc::clone(part)));
                    children.push(id);
                }
                if let Some(k) = kids.get(i) {
                    children.push(*k);
                }
            }
            if parts.is_empty() {
                children.extend_from_slice(kids);
            } else if kids.len() > parts.len() {
                children.extend_from_slice(&kids[parts.len()..]);
            }
            for (old, _) in &c.runs {
                if !runs.iter().any(|(r, _)| r == old) {
                    gone.push(*old);
                }
            }
            c.runs = runs;
            let nothing_to_say = c.kids.is_none() && c.runs.is_empty() && c.sent.is_none();
            if !nothing_to_say && c.sent.as_deref() != Some(children.as_slice()) {
                c.sent = Some(children.clone());
                lists.push(DocDelta::Children {
                    parent: *parent,
                    children: children.into_iter().collect(),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use gorca_model::StateSet;

    use super::*;

    fn node(id: u64, text: Option<&str>) -> SrcNode {
        SrcNode {
            id: SrcNodeId(id),
            role: Role::Paragraph,
            states: StateSet::empty(),
            name: None,
            text: text.map(Arc::from),
            attrs: SmallVec::new(),
            local: None,
        }
    }

    fn kids(ids: &[u64]) -> SmallVec<[SrcNodeId; 8]> {
        ids.iter().map(|i| SrcNodeId(*i)).collect()
    }

    fn children_of(out: &[DocDelta], parent: u64) -> Option<Vec<SrcNodeId>> {
        out.iter().rev().find_map(|d| match d {
            DocDelta::Children {
                parent: p,
                children,
            } if p.0 == parent => Some(children.to_vec()),
            DocDelta::Reset { .. }
            | DocDelta::Upsert(_)
            | DocDelta::Children { .. }
            | DocDelta::Text { .. }
            | DocDelta::Removed(_)
            | DocDelta::Settled { .. }
            | DocDelta::Hint(_) => None,
        })
    }

    fn upserted(out: &[DocDelta]) -> Vec<SrcNode> {
        out.iter()
            .filter_map(|d| match d {
                DocDelta::Upsert(v) => Some(v.clone()),
                DocDelta::Reset { .. }
                | DocDelta::Children { .. }
                | DocDelta::Text { .. }
                | DocDelta::Removed(_)
                | DocDelta::Settled { .. }
                | DocDelta::Hint(_) => None,
            })
            .flatten()
            .collect()
    }

    #[test]
    fn hypertext_is_split_at_objects_and_interleaved_with_children() {
        let mut l = RunLayer::new();
        let out = l.process(vec![
            DocDelta::Upsert(vec![node(1, Some("Hello \u{FFFC} and \u{FFFC}!"))]),
            DocDelta::Upsert(vec![node(2, Some("link")), node(3, Some("other"))]),
            DocDelta::Children {
                parent: SrcNodeId(1),
                children: kids(&[2, 3]),
            },
            DocDelta::Settled { batch: 1 },
        ]);
        let up = upserted(&out);
        let texts: Vec<_> = up
            .iter()
            .filter(|n| is_run(n.id))
            .filter_map(|n| n.text.as_deref())
            .collect();
        assert!(texts.contains(&"Hello "));
        assert!(texts.contains(&" and "));
        assert!(texts.contains(&"!"));
        let list = children_of(&out, 1).unwrap_or_default();
        assert_eq!(list.len(), 5);
        assert_eq!(list[1], SrcNodeId(2));
        assert_eq!(list[3], SrcNodeId(3));
        assert!(is_run(list[0]) && is_run(list[2]) && is_run(list[4]));
        // Node 2 is a leaf container: its text became one run, so it got a list of its own.
        let l2 = children_of(&out, 2).unwrap_or_default();
        assert_eq!(l2.len(), 1);
        assert!(is_run(l2[0]));
        assert!(
            up.iter().all(|n| n.text.is_none() || is_run(n.id)),
            "container text is taken out of the nodes"
        );
        assert!(matches!(out.last(), Some(DocDelta::Settled { batch: 1 })));
    }

    #[test]
    fn an_unchanged_run_is_not_resent_and_an_edit_is_in_place() {
        let mut l = RunLayer::new();
        let first = l.process(vec![DocDelta::Upsert(vec![node(1, Some("alpha"))])]);
        let run = children_of(&first, 1).and_then(|c| c.first().copied());
        let run = run.unwrap_or(SrcNodeId(0));
        assert!(is_run(run));
        // Same text again: no run upsert, no new list.
        let again = l.process(vec![DocDelta::Upsert(vec![node(1, Some("alpha"))])]);
        assert!(upserted(&again).iter().all(|n| !is_run(n.id)));
        assert!(children_of(&again, 1).is_none());
        // An edit keeps the run id.
        let edited = l.process(vec![DocDelta::Text {
            node: SrcNodeId(1),
            edit: TextEdit::Replace(Arc::from("beta")),
        }]);
        let up = upserted(&edited);
        assert_eq!(up.len(), 1);
        assert_eq!(up[0].id, run);
        assert_eq!(up[0].text.as_deref(), Some("beta"));
        assert!(children_of(&edited, 1).is_none(), "list did not change");
    }

    #[test]
    fn deleting_all_text_removes_the_runs() {
        let mut l = RunLayer::new();
        l.process(vec![DocDelta::Upsert(vec![node(1, Some("x\u{FFFC}y"))])]);
        let out = l.process(vec![DocDelta::Text {
            node: SrcNodeId(1),
            edit: TextEdit::Replace(Arc::from("")),
        }]);
        let removed: Vec<_> = out
            .iter()
            .filter_map(|d| match d {
                DocDelta::Removed(v) => Some(v.len()),
                DocDelta::Reset { .. }
                | DocDelta::Upsert(_)
                | DocDelta::Children { .. }
                | DocDelta::Text { .. }
                | DocDelta::Settled { .. }
                | DocDelta::Hint(_) => None,
            })
            .collect();
        assert_eq!(removed, vec![2]);
        assert_eq!(children_of(&out, 1), Some(Vec::new()));
    }

    #[test]
    fn phantoms_are_stripped_and_mismatched_counts_pair_by_position() {
        assert_eq!(&*clean_text("a\u{1F600}\u{FEFF}b"), "a\u{1F600}b");
        let mut l = RunLayer::new();
        // One separator, three children: the surplus follows the last run.
        let out = l.process(vec![
            DocDelta::Upsert(vec![node(1, Some("a\u{FFFC}b"))]),
            DocDelta::Children {
                parent: SrcNodeId(1),
                children: kids(&[10, 11, 12]),
            },
        ]);
        let list = children_of(&out, 1).unwrap_or_default();
        assert_eq!(list.len(), 5);
        assert_eq!(&list[1..2], &[SrcNodeId(10)]);
        assert_eq!(&list[3..], &[SrcNodeId(11), SrcNodeId(12)]);
        // Separators without children are ignored.
        let out = l.process(vec![DocDelta::Upsert(vec![node(
            2,
            Some("p\u{FFFC}q\u{FFFC}r"),
        )])]);
        assert_eq!(children_of(&out, 2).map(|c| c.len()), Some(3));
    }

    #[test]
    fn empty_runs_are_omitted_and_ids_are_deterministic() {
        let mut l = RunLayer::new();
        let out = l.process(vec![
            DocDelta::Upsert(vec![node(1, Some("\u{FFFC}\u{FFFC}"))]),
            DocDelta::Children {
                parent: SrcNodeId(1),
                children: kids(&[5, 6]),
            },
        ]);
        assert_eq!(children_of(&out, 1), Some(vec![SrcNodeId(5), SrcNodeId(6)]));
        assert!(upserted(&out).iter().all(|n| !is_run(n.id)));
        assert_eq!(run_id(SrcNodeId(7), 3), run_id(SrcNodeId(7), 3));
        assert_ne!(run_id(SrcNodeId(7), 3), run_id(SrcNodeId(7), 4));
        assert!(!is_run(SrcNodeId(5)));
        assert!(
            !is_run(SrcNodeId((1 << 62) | 3)),
            "identity synthetic ids are not runs"
        );
    }

    #[test]
    fn reset_forgets_everything() {
        let mut l = RunLayer::new();
        l.process(vec![DocDelta::Upsert(vec![node(1, Some("a"))])]);
        assert_eq!(l.tracked(), 1);
        l.process(vec![DocDelta::Reset {
            root: SrcNodeId(9),
            nav_id: 2,
        }]);
        assert_eq!(l.tracked(), 0);
    }
}
