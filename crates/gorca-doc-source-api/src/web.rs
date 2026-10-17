// SPDX-License-Identifier: GPL-3.0-or-later
//! Neutral web-document exchange format between a `DocSource` and `gorca-doc`.
//!
//! The contract is `gorca_model::DocDelta`; this module adds the two things that contract leaves
//! open for a bulk build:
//!
//! * [`web_attrs`]: well-known `AttrKey`s for the web fields ADR-0003 section 3 wants on a node
//!   (`tag`, DOM `id`, `level`, live-region attributes, ARIA set positions). They are carried in
//!   `SrcNode::attrs` so `gorca-model` does not have to change before `gorca-doc` settles its
//!   node layout. Keys below [`web_attrs::FIRST_DYNAMIC`] are reserved for this table; a source
//!   interns any other attribute name at or above it.
//! * [`TreeBlock`]: a parent-before-child, document-ordered block of nodes plus the complete
//!   ordered child list of every node that has children. [`TreeBlock::into_deltas`] chunks it
//!   into `Upsert` + `Children` deltas so the mirror can apply a 100k-node build in slices
//!   without one long stall (ADR-0003 section 8).
//!
//! Conventions a consumer may rely on (the AT-SPI source in `gorca-doc-source-atspi` follows them):
//!
//! * **Blocks.** A build is one or more blocks. The first block starts with `Reset`; later blocks
//!   of the same build do not. A `Children` list is always complete (SL-4) but may name ids whose
//!   `Upsert` arrives in a later block: such a node is *unfetched* (role `Unknown`, no data) until
//!   then. This is how the top of a page becomes speakable before the rest has arrived.
//! * **Order inside a batch.** `Upsert`s (parents before children), then `Children`, then
//!   `Removed`. A consumer that applies them in that order never sees a dangling parent.
//! * **`text`.** `Upsert` replaces a node wholesale, including `text`. A source that does not
//!   deliver text in its `Upsert`s (the default tier) always sends `None`, and text is read with
//!   `DocRequest::Text`.
//! * **Identity.** A node the page re-created keeps its `SrcNodeId`: the consumer sees an
//!   in-place `Upsert`, not `Removed` + new id (identity ladder variant C, ADR-0003 section 5).
//!   `Removed` therefore means "this logical item is gone".

use gorca_model::{AttrKey, DocDelta, Role, SrcNode, SrcNodeId};
use smallvec::SmallVec;

/// Well-known attribute keys (stable across sources).
pub mod web_attrs {
    use gorca_model::AttrKey;

    /// HTML tag name (`div`, `a`, ...), from the toolkit attribute `tag`.
    pub const TAG: AttrKey = AttrKey(1);
    /// DOM `id`, from the toolkit attribute `id`. Untrusted until validated (identity ladder IL-2).
    pub const DOM_ID: AttrKey = AttrKey(2);
    /// DOM `class`.
    pub const CLASS: AttrKey = AttrKey(3);
    /// ARIA role tokens (`xml-roles`).
    pub const XML_ROLES: AttrKey = AttrKey(4);
    /// Heading or tree level (`level`).
    pub const LEVEL: AttrKey = AttrKey(5);
    /// The node's own `live` attribute (`polite`, `assertive`, `off`): a region root (AM-8).
    pub const LIVE: AttrKey = AttrKey(6);
    /// `container-atomic`.
    pub const ATOMIC: AttrKey = AttrKey(7);
    /// `container-relevant`.
    pub const RELEVANT: AttrKey = AttrKey(8);
    /// `container-busy`.
    pub const BUSY: AttrKey = AttrKey(9);
    /// `posinset` (ARIA set position, TBD-WEB-2).
    pub const POSINSET: AttrKey = AttrKey(10);
    /// `setsize`.
    pub const SETSIZE: AttrKey = AttrKey(11);
    /// Grid positions (TBD-WEB-2).
    pub const ROWINDEX: AttrKey = AttrKey(12);
    pub const COLINDEX: AttrKey = AttrKey(13);
    pub const ROWCOUNT: AttrKey = AttrKey(14);
    pub const COLCOUNT: AttrKey = AttrKey(15);
    /// Raw toolkit role number when the normalised role is `Role::Unknown` (C-4).
    pub const RAW_ROLE: AttrKey = AttrKey(16);
    /// Inherited `container-live`: does not make a node a region root (AM-8).
    pub const CONTAINER_LIVE: AttrKey = AttrKey(17);
    /// CSS `display` of the element (`block`, `inline`, `list-item`, ...), Firefox and Chromium.
    pub const DISPLAY: AttrKey = AttrKey(18);
    /// `layout-guess` (`true` on a table Firefox believes is for layout).
    pub const LAYOUT_GUESS: AttrKey = AttrKey(19);
    /// The accessible description (`aria-describedby`, `aria-description`, `title` as a fallback):
    /// the `Description` property, delivered with the tree so browse mode needs no IPC (M8b).
    /// Synthetic: not an object attribute of any engine.
    pub const DESCRIPTION: AttrKey = AttrKey(20);
    /// The text of the `aria-errormessage` target of an invalid node (relation `error-message`),
    /// read by the source. Synthetic.
    pub const ERROR_TEXT: AttrKey = AttrKey(21);
    /// `aria-current` (`page`, `step`, `location`, `date`, `time`, `true`; `false` is dropped).
    pub const CURRENT: AttrKey = AttrKey(22);
    /// `aria-haspopup` (`true`, `menu`, `listbox`, `tree`, `grid`, `dialog`).
    pub const HASPOPUP: AttrKey = AttrKey(23);
    /// `true` when the author gave the node its name (`aria-label`, `aria-labelledby`, `alt`,
    /// `title`, a `<label>`) instead of the engine computing it from content.
    pub const EXPLICIT_NAME: AttrKey = AttrKey(24);
    /// First key a source may allocate dynamically.
    pub const FIRST_DYNAMIC: u16 = 256;

    /// Canonical name of a well-known key, for reports and logs.
    pub fn name_of(key: AttrKey) -> Option<&'static str> {
        Some(match key {
            TAG => "tag",
            DOM_ID => "id",
            CLASS => "class",
            XML_ROLES => "xml-roles",
            LEVEL => "level",
            LIVE => "live",
            CONTAINER_LIVE => "container-live",
            DISPLAY => "display",
            LAYOUT_GUESS => "layout-guess",
            DESCRIPTION => "description",
            ERROR_TEXT => "error-text",
            CURRENT => "current",
            HASPOPUP => "haspopup",
            EXPLICIT_NAME => "explicit-name",
            ATOMIC => "container-atomic",
            RELEVANT => "container-relevant",
            BUSY => "container-busy",
            POSINSET => "posinset",
            SETSIZE => "setsize",
            ROWINDEX => "rowindex",
            COLINDEX => "colindex",
            ROWCOUNT => "rowcount",
            COLCOUNT => "colcount",
            RAW_ROLE => "raw-role",
            _ => return None,
        })
    }

    /// Map a toolkit attribute name to its well-known key.
    pub fn well_known(name: &str) -> Option<AttrKey> {
        Some(match name {
            "tag" => TAG,
            "id" => DOM_ID,
            "class" => CLASS,
            "xml-roles" => XML_ROLES,
            "level" => LEVEL,
            "live" => LIVE,
            "container-live" => CONTAINER_LIVE,
            "display" => DISPLAY,
            "layout-guess" => LAYOUT_GUESS,
            "description" => DESCRIPTION,
            "error-text" => ERROR_TEXT,
            "current" => CURRENT,
            "haspopup" => HASPOPUP,
            "explicit-name" => EXPLICIT_NAME,
            "container-atomic" | "atomic" => ATOMIC,
            "container-relevant" | "relevant" => RELEVANT,
            "container-busy" | "busy" => BUSY,
            "posinset" => POSINSET,
            "setsize" => SETSIZE,
            "rowindex" => ROWINDEX,
            "colindex" => COLINDEX,
            "rowcount" => ROWCOUNT,
            "colcount" => COLCOUNT,
            _ => return None,
        })
    }
}

/// How complete the nodes of a block are. A source that builds progressively sends the same
/// nodes again with a higher level (an `Upsert` replaces the node by `SrcNodeId`).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub enum Completeness {
    /// Structure, names and attributes only; `role` is `Role::Unknown`, `states` empty.
    Structure,
    /// Structure plus roles.
    Roles,
    /// Roles plus states.
    States,
    /// States plus the text of text-bearing nodes.
    Text,
}

/// A document-ordered block of nodes. `nodes[0]` is the block root; every parent precedes its
/// children (DFS pre-order), so a consumer can allocate in one forward pass.
#[derive(Clone, Debug)]
pub struct TreeBlock {
    pub root: SrcNodeId,
    pub completeness: Completeness,
    pub nodes: Vec<SrcNode>,
    /// Complete ordered child list of every node that has children (SL-4), in `nodes` order.
    pub children: Vec<(SrcNodeId, SmallVec<[SrcNodeId; 8]>)>,
}

impl TreeBlock {
    /// Chunk the block into deltas: `Reset` when `nav_id` is given (a new document), then
    /// `Upsert` slices of at most `chunk` nodes, then every `Children` list. The consumer must
    /// treat the batch as a unit and apply `Children` last so that all ids already exist.
    pub fn into_deltas(self, nav_id: Option<u64>, chunk: usize) -> Vec<DocDelta> {
        let chunk = chunk.max(1);
        let mut out = Vec::with_capacity(2 + self.nodes.len() / chunk + self.children.len());
        if let Some(nav_id) = nav_id {
            out.push(DocDelta::Reset {
                root: self.root,
                nav_id,
            });
        }
        let mut it = self.nodes.into_iter().peekable();
        while it.peek().is_some() {
            out.push(DocDelta::Upsert(it.by_ref().take(chunk).collect()));
        }
        for (parent, children) in self.children {
            out.push(DocDelta::Children { parent, children });
        }
        out
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }
}

/// True for keys a source allocated itself.
/// Roles whose invalid state is announced with their error message. Containers (a form that
/// holds an invalid field) are marked invalid by Gecko too and say nothing.
pub fn takes_error_text(role: Role) -> bool {
    matches!(
        role,
        Role::TextEntry
            | Role::PasswordEntry
            | Role::ComboBox
            | Role::CheckBox
            | Role::RadioButton
            | Role::SpinButton
            | Role::Slider
            | Role::ToggleButton
            | Role::Button
    )
}

pub fn is_dynamic(key: AttrKey) -> bool {
    key.0 >= web_attrs::FIRST_DYNAMIC
}

#[cfg(test)]
mod tests {
    use super::*;
    use gorca_model::{Role, StateSet};

    fn n(id: u64) -> SrcNode {
        SrcNode {
            id: SrcNodeId(id),
            role: Role::Panel,
            states: StateSet::empty(),
            name: None,
            text: None,
            attrs: SmallVec::default(),
            local: None,
        }
    }

    #[test]
    fn block_is_chunked_with_children_last() {
        let b = TreeBlock {
            root: SrcNodeId(1),
            completeness: Completeness::States,
            nodes: (1..=5).map(n).collect(),
            children: vec![(
                SrcNodeId(1),
                [SrcNodeId(2), SrcNodeId(3)].into_iter().collect(),
            )],
        };
        let d = b.into_deltas(Some(7), 2);
        assert!(matches!(d[0], DocDelta::Reset { nav_id: 7, .. }));
        assert!(matches!(&d[1], DocDelta::Upsert(v) if v.len() == 2));
        assert!(matches!(&d[3], DocDelta::Upsert(v) if v.len() == 1));
        assert!(matches!(d.last(), Some(DocDelta::Children { .. })));
    }

    #[test]
    fn well_known_names_resolve() {
        assert_eq!(web_attrs::well_known("tag"), Some(web_attrs::TAG));
        assert_eq!(web_attrs::well_known("nope"), None);
        assert!(!is_dynamic(web_attrs::RAW_ROLE));
    }
}
