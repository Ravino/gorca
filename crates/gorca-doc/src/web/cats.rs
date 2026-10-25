// SPDX-License-Identifier: GPL-3.0-or-later
//! Navigation categories (ADR-0003 section 4.4), node flags, node specs and the pure
//! `(role, level, flags) -> categories` function (DM-4).

use std::ops::BitOr;
use std::sync::Arc;

use gorca_model::sem::SemEntry;
use gorca_model::sem::SemRole;
use gorca_model::{LocalId, SrcNodeId, Value};

/// Set of navigation categories (bit mask). A node's own `Cat` is a pure function of
/// `(role, level, flags)`; queries pass the union they are interested in.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct Cat(pub u64);

impl Cat {
    pub const NONE: Cat = Cat(0);

    pub const HEADING: Cat = Cat::bit(0);
    pub const HEADING_1: Cat = Cat::bit(1);
    pub const HEADING_2: Cat = Cat::bit(2);
    pub const HEADING_3: Cat = Cat::bit(3);
    pub const HEADING_4: Cat = Cat::bit(4);
    pub const HEADING_5: Cat = Cat::bit(5);
    pub const HEADING_6: Cat = Cat::bit(6);
    pub const LINK: Cat = Cat::bit(7);
    pub const LINK_VISITED: Cat = Cat::bit(8);
    pub const LINK_UNVISITED: Cat = Cat::bit(9);
    pub const FORM_FIELD: Cat = Cat::bit(10);
    pub const EDIT: Cat = Cat::bit(11);
    pub const BUTTON: Cat = Cat::bit(12);
    pub const COMBOBOX: Cat = Cat::bit(13);
    pub const CHECKBOX: Cat = Cat::bit(14);
    pub const RADIO: Cat = Cat::bit(15);
    pub const SLIDER_SPIN: Cat = Cat::bit(16);
    pub const LISTBOX: Cat = Cat::bit(17);
    pub const LANDMARK: Cat = Cat::bit(18);
    pub const REGION: Cat = Cat::bit(19);
    pub const ARTICLE: Cat = Cat::bit(20);
    pub const DIALOG: Cat = Cat::bit(21);
    pub const TABLE: Cat = Cat::bit(22);
    pub const LAYOUT_TABLE: Cat = Cat::bit(23);
    pub const LIST: Cat = Cat::bit(24);
    pub const LIST_ITEM: Cat = Cat::bit(25);
    pub const TERM_DEF: Cat = Cat::bit(26);
    pub const GRAPHIC: Cat = Cat::bit(27);
    pub const BLOCKQUOTE: Cat = Cat::bit(28);
    pub const SEPARATOR: Cat = Cat::bit(29);
    pub const FRAME: Cat = Cat::bit(30);
    pub const EMBEDDED: Cat = Cat::bit(31);
    pub const MATH: Cat = Cat::bit(32);
    pub const LIVE_REGION: Cat = Cat::bit(33);
    pub const ALERT: Cat = Cat::bit(34);
    pub const CLICKABLE: Cat = Cat::bit(35);
    pub const FOCUSABLE: Cat = Cat::bit(36);
    pub const ANNOTATION: Cat = Cat::bit(37);
    pub const TEXT_BLOCK: Cat = Cat::bit(38);
    pub const LINE_HOST: Cat = Cat::bit(39);
    pub const TEXT_RUN: Cat = Cat::bit(40);
    /// A node in an invalid state (`aria-invalid`, a spelling or grammar error): quick nav W.
    pub const ERROR: Cat = Cat::bit(41);

    /// "Landmark" navigation unit: landmarks and regions (ADR-0003 section 12).
    pub const EMBEDDED_OR_DIALOG: Cat = Cat(Cat::EMBEDDED.0 | Cat::DIALOG.0);
    /// What `,` and Shift+`,` jump out of: lists, tables, quotes, landmarks, dialogs, articles.
    pub const CONTAINER: Cat = Cat(Cat::LIST.0
        | Cat::TABLE.0
        | Cat::BLOCKQUOTE.0
        | Cat::LANDMARK.0
        | Cat::REGION.0
        | Cat::DIALOG.0
        | Cat::ARTICLE.0);
    pub const ANY_LANDMARK: Cat = Cat(Cat::LANDMARK.0 | Cat::REGION.0);

    pub const fn bit(n: u32) -> Cat {
        Cat(1u64 << n)
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub const fn intersects(self, other: Cat) -> bool {
        self.0 & other.0 != 0
    }

    /// Heading of exactly `level` (1 to 6; other levels give the plain `HEADING` bit only).
    pub const fn heading_level(level: u8) -> Cat {
        match level {
            1 => Cat::HEADING_1,
            2 => Cat::HEADING_2,
            3 => Cat::HEADING_3,
            4 => Cat::HEADING_4,
            5 => Cat::HEADING_5,
            6 => Cat::HEADING_6,
            _ => Cat::NONE,
        }
    }

    /// Headings of level `1..=max` (level filter "up to h`max`").
    pub const fn headings_up_to(max: u8) -> Cat {
        let mut bits = 0u64;
        let mut k = 1u8;
        while k <= max && k <= 6 {
            bits |= Cat::heading_level(k).0;
            k += 1;
        }
        Cat(bits)
    }
}

impl BitOr for Cat {
    type Output = Cat;
    fn bitor(self, rhs: Cat) -> Cat {
        Cat(self.0 | rhs.0)
    }
}

/// Number of categories with per-subtree counts ("k of N", `kth`, `rank`, `count`).
pub const RANKED_N: usize = 16;

/// The ranked categories, in count-array order. Each is a single-bit `Cat`.
pub const RANKED: [Cat; RANKED_N] = [
    Cat::HEADING,
    Cat::HEADING_1,
    Cat::HEADING_2,
    Cat::HEADING_3,
    Cat::HEADING_4,
    Cat::HEADING_5,
    Cat::HEADING_6,
    Cat::LINK,
    Cat::FORM_FIELD,
    Cat::BUTTON,
    Cat::EDIT,
    Cat::LANDMARK,
    Cat::TABLE,
    Cat::LIST,
    Cat::LIST_ITEM,
    Cat::REGION,
];

/// Index of a single-bit ranked category in the count array.
pub fn ranked_index(c: Cat) -> Option<usize> {
    RANKED.iter().position(|r| *r == c)
}

/// Node flags that are not derivable from the role.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct NodeFlags(pub u32);

impl NodeFlags {
    pub const NONE: NodeFlags = NodeFlags(0);
    /// Not presentable: no categories, no flat text of its own (children are the caller's job).
    pub const HIDDEN: NodeFlags = NodeFlags(1 << 0);
    pub const VISITED: NodeFlags = NodeFlags(1 << 1);
    pub const LAYOUT: NodeFlags = NodeFlags(1 << 2);
    /// A landmark role that is a plain `region` (navigated, but reported as a region).
    pub const REGION: NodeFlags = NodeFlags(1 << 3);
    pub const ARTICLE: NodeFlags = NodeFlags(1 << 4);
    pub const LIVE: NodeFlags = NodeFlags(1 << 5);
    pub const BLOCKQUOTE: NodeFlags = NodeFlags(1 << 6);
    pub const CLICKABLE: NodeFlags = NodeFlags(1 << 7);
    pub const FOCUSABLE: NodeFlags = NodeFlags(1 << 8);
    /// A hard line break follows this node's own text (block boundary).
    pub const BREAK_AFTER: NodeFlags = NodeFlags(1 << 9);
    pub const LISTBOX: NodeFlags = NodeFlags(1 << 10);
    /// Block-level box: contributes a line break before its own text (FT-4).
    pub const BLOCK: NodeFlags = NodeFlags(1 << 11);
    /// A line break precedes this node's own text: the node is `BLOCK`, or its previous visible
    /// sibling is (FT-4). Set by the translator, recomputed for a parent's children whenever its
    /// child list changes.
    pub const BREAK_BEFORE: NodeFlags = NodeFlags(1 << 12);
    /// A frame or a nested document (`Cat::FRAME`, quick nav M).
    pub const FRAME: NodeFlags = NodeFlags(1 << 13);
    /// A table cell or header cell (a row reads as one line: cells are inline, FT-4).
    pub const CELL: NodeFlags = NodeFlags(1 << 14);
    /// A single space precedes this node's own text: it is a cell that follows another node in
    /// its row. Set by the translator like `BREAK_BEFORE`, never together with it.
    pub const CELL_SEP: NodeFlags = NodeFlags(1 << 15);
    /// An annotation: inserted or deleted text, a comment (quick nav A).
    pub const ANNOTATION: NodeFlags = NodeFlags(1 << 16);
    /// The node is in an invalid state (quick nav W).
    pub const ERROR: NodeFlags = NodeFlags(1 << 17);

    pub const fn contains(self, other: NodeFlags) -> bool {
        self.0 & other.0 == other.0
    }

    /// Scalars a visible node contributes ahead of its own text: a line break or a cell gap.
    pub const fn lead_len(self) -> u32 {
        if self.0 & (NodeFlags::HIDDEN.0) != 0 {
            0
        } else if self.0 & (NodeFlags::BREAK_BEFORE.0 | NodeFlags::CELL_SEP.0) != 0 {
            1
        } else {
            0
        }
    }
}

impl BitOr for NodeFlags {
    type Output = NodeFlags;
    fn bitor(self, rhs: NodeFlags) -> NodeFlags {
        NodeFlags(self.0 | rhs.0)
    }
}

/// DM-4: categories are a pure function of `(role, level, flags)`.
pub fn cats_of(role: SemRole, level: u8, flags: NodeFlags) -> Cat {
    if flags.contains(NodeFlags::HIDDEN) {
        return Cat::NONE;
    }
    let mut c = match role {
        SemRole::Heading => Cat::HEADING | Cat::heading_level(level),
        SemRole::Link => {
            let v = if flags.contains(NodeFlags::VISITED) {
                Cat::LINK_VISITED
            } else {
                Cat::LINK_UNVISITED
            };
            Cat::LINK | v | Cat::FOCUSABLE | Cat::CLICKABLE
        }
        SemRole::Button | SemRole::Switch => Cat::FORM_FIELD | Cat::BUTTON | Cat::FOCUSABLE,
        SemRole::Checkbox | SemRole::MenuItemCheckbox => {
            Cat::FORM_FIELD | Cat::CHECKBOX | Cat::FOCUSABLE
        }
        SemRole::Radio | SemRole::MenuItemRadio => Cat::FORM_FIELD | Cat::RADIO | Cat::FOCUSABLE,
        SemRole::Combobox => Cat::FORM_FIELD | Cat::COMBOBOX | Cat::FOCUSABLE,
        SemRole::TextBox | SemRole::SearchBox | SemRole::PasswordBox => {
            Cat::FORM_FIELD | Cat::EDIT | Cat::FOCUSABLE
        }
        SemRole::SpinButton | SemRole::Slider => {
            Cat::FORM_FIELD | Cat::SLIDER_SPIN | Cat::FOCUSABLE
        }
        SemRole::Region
        | SemRole::Banner
        | SemRole::Complementary
        | SemRole::ContentInfo
        | SemRole::Main
        | SemRole::Navigation
        | SemRole::Search
        | SemRole::Form => {
            if flags.contains(NodeFlags::REGION) {
                Cat::REGION
            } else {
                Cat::LANDMARK
            }
        }
        SemRole::Dialog | SemRole::AlertDialog => Cat::DIALOG,
        SemRole::Application => Cat::EMBEDDED,
        SemRole::Alert => Cat::ALERT | Cat::LIVE_REGION,
        SemRole::Table | SemRole::Grid | SemRole::TreeGrid | SemRole::Presentation
            if flags.contains(NodeFlags::LAYOUT) =>
        {
            Cat::LAYOUT_TABLE
        }
        SemRole::Table | SemRole::Grid | SemRole::TreeGrid => Cat::TABLE,
        SemRole::List | SemRole::Listbox => Cat::LIST,
        SemRole::ListItem | SemRole::Option => Cat::LIST_ITEM,
        SemRole::Img => Cat::GRAPHIC,
        SemRole::Separator => Cat::SEPARATOR,
        SemRole::Paragraph => Cat::TEXT_BLOCK,
        SemRole::StaticText
        | SemRole::Emphasis
        | SemRole::Strong
        | SemRole::Mark
        | SemRole::Code
        | SemRole::Time
        | SemRole::Insertion
        | SemRole::Deletion
        | SemRole::Subscript
        | SemRole::Superscript => Cat::TEXT_RUN,
        SemRole::Unknown
        | SemRole::Generic
        | SemRole::Article
        | SemRole::Blockquote
        | SemRole::Caption
        | SemRole::Cell
        | SemRole::ColumnHeader
        | SemRole::Comment
        | SemRole::Definition
        | SemRole::DescriptionList
        | SemRole::Details
        | SemRole::Document
        | SemRole::DocumentWeb
        | SemRole::Feed
        | SemRole::Figure
        | SemRole::Group
        | SemRole::Log
        | SemRole::Marquee
        | SemRole::Math
        | SemRole::Menu
        | SemRole::Menubar
        | SemRole::MenuItem
        | SemRole::Meter
        | SemRole::Note
        | SemRole::Presentation
        | SemRole::ProgressBar
        | SemRole::RadioGroup
        | SemRole::Row
        | SemRole::RowGroup
        | SemRole::RowHeader
        | SemRole::Scrollbar
        | SemRole::Status
        | SemRole::Suggestion
        | SemRole::Summary
        | SemRole::Tab
        | SemRole::TabList
        | SemRole::TabPanel
        | SemRole::Term
        | SemRole::Timer
        | SemRole::Toolbar
        | SemRole::Tooltip
        | SemRole::Tree
        | SemRole::TreeItem
        | SemRole::Terminal
        | SemRole::Window
        | SemRole::Popover
        | SemRole::Iframe
        | SemRole::GridCell => Cat::NONE,
    };
    if flags.contains(NodeFlags::ARTICLE) {
        c = c | Cat::ARTICLE;
    }
    if flags.contains(NodeFlags::LIVE) {
        c = c | Cat::LIVE_REGION;
    }
    if flags.contains(NodeFlags::BLOCKQUOTE) {
        c = c | Cat::BLOCKQUOTE;
    }
    if flags.contains(NodeFlags::CLICKABLE) {
        c = c | Cat::CLICKABLE;
    }
    if flags.contains(NodeFlags::FOCUSABLE) {
        c = c | Cat::FOCUSABLE;
    }
    if flags.contains(NodeFlags::LISTBOX) {
        c = c | Cat::LISTBOX;
    }
    if flags.contains(NodeFlags::FRAME) {
        c = c | Cat::FRAME;
    }
    if flags.contains(NodeFlags::ANNOTATION) {
        c = c | Cat::ANNOTATION;
    }
    if flags.contains(NodeFlags::ERROR) {
        c = c | Cat::ERROR;
    }
    c
}

/// What the mirror keeps about a node besides what the treap needs (ADR-0003 AM-6, browse-mode
/// design TR-6): the source identity, the transport key for the focus join, the contents of a
/// field and the fused semantics. Every presentation fact (states, description, attributes) is a
/// group of the fused entry (ADR-0022 section 8.1); categories stay a pure function of
/// `(role, level, flags)`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NodeData {
    pub src: Option<SrcNodeId>,
    pub local: Option<LocalId>,
    pub value: Option<Value>,
    /// The page itself made the node an article (its DOM tag is `article`): the flag stays when
    /// an add-on patch takes the fused role away.
    pub article_tag: bool,
    /// The fused semantics of the node (ADR-0022 SM-8: produced by `FUSE` in the document actor,
    /// the node's single writer, before the batch is published). `None` until the node was
    /// fused, for synthetic nodes and for sources that do not normalise. Read it through the
    /// mirror only; nothing else merges sources.
    pub sem: Option<Arc<SemEntry>>,
}

/// Everything a source supplies for one node.
#[derive(Clone, Debug)]
pub struct NodeSpec {
    pub role: SemRole,
    /// Heading level 1 to 6 (ignored for other roles).
    pub level: u8,
    pub flags: NodeFlags,
    /// Opaque identity signature, e.g. `xxh3(role, name, tag)` (ADR-0003 IL-3).
    pub sig: u64,
    pub name: Option<Arc<str>>,
    /// Own text (scalar-indexed). Contributes to the flat text in preorder.
    pub text: Option<Arc<str>>,
    pub data: Option<Arc<NodeData>>,
}

impl NodeSpec {
    pub fn new(role: impl Into<SemRole>) -> Self {
        NodeSpec {
            role: role.into(),
            level: 0,
            flags: NodeFlags::NONE,
            sig: 0,
            name: None,
            text: None,
            data: None,
        }
    }

    #[must_use]
    pub fn data(mut self, data: Arc<NodeData>) -> Self {
        self.data = Some(data);
        self
    }

    #[must_use]
    pub fn level(mut self, level: u8) -> Self {
        self.level = level;
        self
    }

    #[must_use]
    pub fn flags(mut self, flags: NodeFlags) -> Self {
        self.flags = self.flags | flags;
        self
    }

    #[must_use]
    pub fn sig(mut self, sig: u64) -> Self {
        self.sig = sig;
        self
    }

    #[must_use]
    pub fn name(mut self, name: &str) -> Self {
        self.name = Some(Arc::from(name));
        self
    }

    #[must_use]
    pub fn text(mut self, text: &str) -> Self {
        self.text = Some(Arc::from(text));
        self
    }
}

/// A node with its children, in document order. Inserted as one operation.
#[derive(Clone, Debug)]
pub struct SubtreeSpec {
    pub node: NodeSpec,
    pub children: Vec<SubtreeSpec>,
}

impl SubtreeSpec {
    pub fn leaf(node: NodeSpec) -> Self {
        SubtreeSpec {
            node,
            children: Vec::new(),
        }
    }

    pub fn with_children(node: NodeSpec, children: Vec<SubtreeSpec>) -> Self {
        SubtreeSpec { node, children }
    }

    /// Number of nodes in the subtree.
    pub fn count(&self) -> usize {
        let mut n = 0;
        let mut stack = vec![self];
        while let Some(s) = stack.pop() {
            n += 1;
            stack.extend(s.children.iter());
        }
        n
    }
}

/// Partial update of a node's properties (attribute change).
#[derive(Clone, Debug, Default)]
pub struct PropsPatch {
    pub role: Option<SemRole>,
    pub level: Option<u8>,
    pub flags: Option<NodeFlags>,
    pub sig: Option<u64>,
    pub name: Option<Option<Arc<str>>>,
    pub data: Option<Option<Arc<NodeData>>>,
}

#[cfg(test)]
mod tests {
    #[test]
    fn landmark_roles_are_regions_only_with_the_region_flag_and_tables_follow_the_layout_flag() {
        for r in [
            SemRole::Region,
            SemRole::Banner,
            SemRole::Main,
            SemRole::Form,
        ] {
            assert!(
                cats_of(r, 0, NodeFlags::NONE).intersects(Cat::LANDMARK),
                "{r:?}"
            );
            assert!(
                cats_of(r, 0, NodeFlags::REGION).intersects(Cat::REGION),
                "{r:?}"
            );
        }
        assert!(cats_of(SemRole::Table, 0, NodeFlags::NONE).intersects(Cat::TABLE));
        assert!(cats_of(SemRole::Presentation, 0, NodeFlags::LAYOUT).intersects(Cat::LAYOUT_TABLE));
        assert!(cats_of(SemRole::Presentation, 0, NodeFlags::NONE).is_empty());
        assert!(cats_of(SemRole::Option, 0, NodeFlags::NONE).intersects(Cat::LIST_ITEM));
    }

    use super::*;

    #[test]
    fn heading_cats_carry_level() {
        let c = cats_of(SemRole::Heading, 3, NodeFlags::NONE);
        assert!(c.intersects(Cat::HEADING));
        assert!(c.intersects(Cat::HEADING_3));
        assert!(!c.intersects(Cat::HEADING_2));
        assert!(Cat::headings_up_to(3).intersects(c));
        assert!(!Cat::headings_up_to(2).intersects(Cat::heading_level(3)));
    }

    #[test]
    fn hidden_has_no_cats() {
        assert!(cats_of(SemRole::Link, 0, NodeFlags::HIDDEN).is_empty());
    }

    #[test]
    fn ranked_are_single_bits_and_unique() {
        for (i, c) in RANKED.iter().enumerate() {
            assert_eq!(c.0.count_ones(), 1);
            assert_eq!(ranked_index(*c), Some(i));
        }
    }
}
