// SPDX-License-Identifier: GPL-3.0-or-later
//! Deterministic synthetic page generator for tests and the MB benches: a document with a
//! navigation landmark, regions of headings, paragraphs (text runs and links), lists, tables
//! and forms, shaped like a long article page.

use gorca_model::sem::SemRole;

use super::cats::{NodeFlags, NodeSpec, SubtreeSpec};

/// Small deterministic generator (xorshift64*), shared with the property tests.
#[derive(Clone, Debug)]
pub struct Rng(pub u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in `0..n` (`n > 0`; returns 0 for `n == 0`).
    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next_u64() % n as u64) as usize
        }
    }
}

const WORDS: [&str; 12] = [
    "alpha", "beta", "gamma", "delta", "reader", "screen", "focus", "page", "link", "table",
    "navigate", "speech",
];

fn sentence(rng: &mut Rng, words: usize) -> String {
    let mut s = String::new();
    for i in 0..words {
        if i > 0 {
            s.push(' ');
        }
        s.push_str(WORDS[rng.below(WORDS.len())]);
    }
    s
}

fn text(rng: &mut Rng, n: usize) -> SubtreeSpec {
    SubtreeSpec::leaf(NodeSpec::new(SemRole::StaticText).text(&sentence(rng, n)))
}

fn link(rng: &mut Rng, id: u64) -> SubtreeSpec {
    let label = sentence(rng, 2);
    let mut flags = NodeFlags::NONE;
    if id.is_multiple_of(5) {
        flags = flags | NodeFlags::VISITED;
    }
    SubtreeSpec::with_children(
        NodeSpec::new(SemRole::Link)
            .flags(flags)
            .name(&label)
            .sig(id),
        vec![SubtreeSpec::leaf(
            NodeSpec::new(SemRole::StaticText).text(&label),
        )],
    )
}

fn paragraph(rng: &mut Rng, id: u64) -> SubtreeSpec {
    let a = text(rng, 6);
    let l = link(rng, id);
    let b = text(rng, 5);
    SubtreeSpec::with_children(
        NodeSpec::new(SemRole::Paragraph)
            .flags(NodeFlags::BREAK_AFTER)
            .sig(id),
        vec![a, l, b],
    )
}

/// Generate a page with at least `target` nodes (it overshoots by at most one section).
pub fn page(target: usize, seed: u64) -> SubtreeSpec {
    let mut rng = Rng::new(seed);
    let mut count = 1usize; // the document node
    let mut children: Vec<SubtreeSpec> = Vec::new();
    let mut id = 1u64;

    let push = |children: &mut Vec<SubtreeSpec>, s: SubtreeSpec, count: &mut usize| {
        *count += s.count();
        children.push(s);
    };

    // Banner with a navigation list.
    let items: Vec<SubtreeSpec> = (0..8)
        .map(|_| {
            id += 1;
            SubtreeSpec::with_children(NodeSpec::new(SemRole::ListItem), vec![link(&mut rng, id)])
        })
        .collect();
    let nav = SubtreeSpec::with_children(
        NodeSpec::new(SemRole::Region).name("banner"),
        vec![SubtreeSpec::with_children(
            NodeSpec::new(SemRole::List),
            items,
        )],
    );
    push(&mut children, nav, &mut count);

    let mut section = 0u64;
    let mut main: Vec<SubtreeSpec> = Vec::new();
    let mut main_count = 0usize;
    while count + main_count < target {
        section += 1;
        let mut body: Vec<SubtreeSpec> = Vec::new();
        let level = 1 + (section % 4) as u8;
        id += 1;
        body.push(SubtreeSpec::with_children(
            NodeSpec::new(SemRole::Heading)
                .level(level)
                .flags(NodeFlags::BREAK_AFTER)
                .name("section")
                .sig(id),
            vec![text(&mut rng, 3)],
        ));
        for _ in 0..(3 + rng.below(4)) {
            id += 1;
            body.push(paragraph(&mut rng, id));
        }
        if section.is_multiple_of(4) {
            let li: Vec<SubtreeSpec> = (0..5)
                .map(|_| {
                    id += 1;
                    SubtreeSpec::with_children(
                        NodeSpec::new(SemRole::ListItem).flags(NodeFlags::BREAK_AFTER),
                        vec![link(&mut rng, id)],
                    )
                })
                .collect();
            body.push(SubtreeSpec::with_children(NodeSpec::new(SemRole::List), li));
        }
        if section.is_multiple_of(7) {
            let rows: Vec<SubtreeSpec> = (0..4)
                .map(|_| {
                    let cells: Vec<SubtreeSpec> = (0..3)
                        .map(|_| {
                            SubtreeSpec::with_children(
                                NodeSpec::new(SemRole::Cell),
                                vec![text(&mut rng, 2)],
                            )
                        })
                        .collect();
                    SubtreeSpec::with_children(
                        NodeSpec::new(SemRole::Row).flags(NodeFlags::BREAK_AFTER),
                        cells,
                    )
                })
                .collect();
            body.push(SubtreeSpec::with_children(
                NodeSpec::new(SemRole::Table),
                rows,
            ));
        }
        if section.is_multiple_of(5) {
            body.push(SubtreeSpec::with_children(
                NodeSpec::new(SemRole::Generic).flags(NodeFlags::BREAK_AFTER),
                vec![
                    SubtreeSpec::leaf(NodeSpec::new(SemRole::TextBox).name("query")),
                    SubtreeSpec::leaf(NodeSpec::new(SemRole::Checkbox).name("exact")),
                    SubtreeSpec::leaf(NodeSpec::new(SemRole::Button).name("go")),
                ],
            ));
        }
        let region = SubtreeSpec::with_children(
            NodeSpec::new(SemRole::Region)
                .flags(NodeFlags::REGION)
                .name("section")
                .sig(section),
            body,
        );
        main_count += region.count();
        main.push(region);
    }
    children.push(SubtreeSpec::with_children(
        NodeSpec::new(SemRole::Region).name("main"),
        main,
    ));
    SubtreeSpec::with_children(NodeSpec::new(SemRole::Document).name("page"), children)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::web::cats::Cat;
    use crate::web::{DocOp, WebDoc};

    #[test]
    fn page_builds_and_validates() {
        let spec = page(5_000, 42);
        assert!(spec.count() >= 5_000);
        let mut d = WebDoc::new(9);
        let out = d.apply(&DocOp::InsertSubtree {
            parent: None,
            before: None,
            subtree: Arc::new(spec.clone()),
        });
        assert!(out.is_ok());
        assert_eq!(d.len(), spec.count());
        assert_eq!(d.validate(), Ok(()));
        assert!(d.count(Cat::HEADING).is_some_and(|n| n > 10));
        assert!(d.first_of(Cat::LINK).is_some());
        // Same seed, same page.
        assert_eq!(page(5_000, 42).count(), spec.count());
    }
}
