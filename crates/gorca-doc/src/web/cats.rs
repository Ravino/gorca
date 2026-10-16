//! Navigation categories (ADR-0003 section 4.4), node flags, node specs and the pure
//! `(role, level, flags) -> categories` function (DM-4).

use std::ops::BitOr;
use std::sync::Arc;

use gorca_model::{AttrMap, LocalId, Role, SrcNodeId, StateSet, Value};

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
pub fn cats_of(role: Role, level: u8, flags: NodeFlags) -> Cat {
    if flags.contains(NodeFlags::HIDDEN) {
        return Cat::NONE;
    }
    let mut c = match role {
        Role::Heading => Cat::HEADING | Cat::heading_level(level),
        Role::Link => {
            let v = if flags.contains(NodeFlags::VISITED) {
                Cat::LINK_VISITED
            } else {
                Cat::LINK_UNVISITED
            };
            Cat::LINK | v | Cat::FOCUSABLE | Cat::CLICKABLE
        }
        Role::Button | Role::ToggleButton => Cat::FORM_FIELD | Cat::BUTTON | Cat::FOCUSABLE,
        Role::CheckBox => Cat::FORM_FIELD | Cat::CHECKBOX | Cat::FOCUSABLE,
        Role::RadioButton => Cat::FORM_FIELD | Cat::RADIO | Cat::FOCUSABLE,
        Role::ComboBox => Cat::FORM_FIELD | Cat::COMBOBOX | Cat::FOCUSABLE,
        Role::TextEntry | Role::PasswordEntry => Cat::FORM_FIELD | Cat::EDIT | Cat::FOCUSABLE,
        Role::SpinButton | Role::Slider => Cat::FORM_FIELD | Cat::SLIDER_SPIN | Cat::FOCUSABLE,
        Role::Landmark => {
            if flags.contains(NodeFlags::REGION) {
                Cat::REGION
            } else {
                Cat::LANDMARK
            }
        }
        Role::Dialog => Cat::DIALOG,
        Role::Application => Cat::EMBEDDED,
        Role::Alert => Cat::ALERT | Cat::LIVE_REGION,
        Role::Table => {
            if flags.contains(NodeFlags::LAYOUT) {
                Cat::LAYOUT_TABLE
            } else {
                Cat::TABLE
            }
        }
        Role::List => Cat::LIST,
        Role::ListItem => Cat::LIST_ITEM,
        Role::Image => Cat::GRAPHIC,
        Role::Separator => Cat::SEPARATOR,
        Role::Paragraph => Cat::TEXT_BLOCK,
        Role::StaticText => Cat::TEXT_RUN,
        Role::Unknown
        | Role::Window
        | Role::Panel
        | Role::Document
        | Role::DocumentWeb
        | Role::Row
        | Role::Cell
        | Role::ColumnHeader
        | Role::RowHeader
        | Role::ProgressBar
        | Role::Menu
        | Role::MenuBar
        | Role::MenuItem
        | Role::Popover
        | Role::Tab
        | Role::TabList
        | Role::Tree
        | Role::TreeItem
        | Role::Toolbar
        | Role::StatusBar
        | Role::Label
        | Role::Terminal => Cat::NONE,
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
/// design TR-6): the source identity, the transport key for the focus join, states, description,
/// value and the attribute map. Categories stay a pure function of `(role, level, flags)`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NodeMeta {
    pub src: Option<SrcNodeId>,
    pub local: Option<LocalId>,
    pub states: StateSet,
    pub description: Option<Arc<str>>,
    pub value: Option<Value>,
    pub attrs: AttrMap,
}

/// Everything a source supplies for one node.
#[derive(Clone, Debug)]
pub struct NodeSpec {
    pub role: Role,
    /// Heading level 1 to 6 (ignored for other roles).
    pub level: u8,
    pub flags: NodeFlags,
    /// Opaque identity signature, e.g. `xxh3(role, name, tag)` (ADR-0003 IL-3).
    pub sig: u64,
    pub name: Option<Arc<str>>,
    /// Own text (scalar-indexed). Contributes to the flat text in preorder.
    pub text: Option<Arc<str>>,
    pub meta: Option<Arc<NodeMeta>>,
}

impl NodeSpec {
    pub fn new(role: Role) -> Self {
        NodeSpec {
            role,
            level: 0,
            flags: NodeFlags::NONE,
            sig: 0,
            name: None,
            text: None,
            meta: None,
        }
    }

    #[must_use]
    pub fn meta(mut self, meta: Arc<NodeMeta>) -> Self {
        self.meta = Some(meta);
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
    pub role: Option<Role>,
    pub level: Option<u8>,
    pub flags: Option<NodeFlags>,
    pub sig: Option<u64>,
    pub name: Option<Option<Arc<str>>>,
    pub meta: Option<Option<Arc<NodeMeta>>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heading_cats_carry_level() {
        let c = cats_of(Role::Heading, 3, NodeFlags::NONE);
        assert!(c.intersects(Cat::HEADING));
        assert!(c.intersects(Cat::HEADING_3));
        assert!(!c.intersects(Cat::HEADING_2));
        assert!(Cat::headings_up_to(3).intersects(c));
        assert!(!Cat::headings_up_to(2).intersects(Cat::heading_level(3)));
    }

    #[test]
    fn hidden_has_no_cats() {
        assert!(cats_of(Role::Link, 0, NodeFlags::HIDDEN).is_empty());
    }

    #[test]
    fn ranked_are_single_bits_and_unique() {
        for (i, c) in RANKED.iter().enumerate() {
            assert_eq!(c.0.count_ones(), 1);
            assert_eq!(ranked_index(*c), Some(i));
        }
    }
}
