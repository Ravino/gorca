// SPDX-License-Identifier: GPL-3.0-or-later
//! The seam the golden tests of review (`gorca-connector-atspi`, M22b3) call: the fused-entry
//! zone words, the navigation class and the object moves over a page of recorded snapshots. The
//! cut-over gate that compared them with the legacy paths is gone (tag `legacy/review-last`);
//! the goldens in `tests/fixtures/sem/golden/review.{en,ru}.tsv` are what guards them now.

use std::collections::HashMap;
use std::sync::Arc;

use gorca_model::{Lookup, NodeKey, NodeSnapshot, NodeView, PropMask};

pub use super::collect::{is_surface_role, wants_lines};
pub use super::model::ReviewLimits;
pub use super::objnav::{NavMove, NavSource, PresClass, StepResult, class_of_snapshot, step};
pub use super::zone::{join_tokens, object_tokens, role_word, zone_tokens};

/// Whether the zone of an object is made even when it says nothing: it can be focused or acted on.
pub fn actionable(snap: &NodeSnapshot) -> bool {
    snap.struct_state()
        .contains(gorca_model::StructFlags::FOCUSABLE)
        || snap.caps().has_action()
}

/// What the text layer tracks of a node: (editable, secret, multi-line), `None` for a node it does
/// not follow. The recorded-corpus goldens of the text layer read it.
pub fn text_tracked(snap: &NodeSnapshot) -> Option<(bool, bool, bool)> {
    crate::text::tracked_bits(snap)
}

/// The parts a node plays in the "where am I" answer: (collection, container, window,
/// application). The recorded-corpus goldens of the info commands read it.
pub fn info_shape(snap: &NodeSnapshot) -> (bool, bool, bool, bool) {
    crate::commands::info::shape_of(snap)
}

/// How a node takes part in a dialog read (`text`, `container`, `skip`) and whether the window
/// reads its content on open. The recorded-corpus goldens of the window surface read it.
pub fn dialog_part(snap: &NodeSnapshot) -> (&'static str, bool) {
    (
        crate::present::part_name(snap),
        crate::present::reads_content(crate::nodekind::role_of(snap)),
    )
}

/// How the present layer treats a node (the recorded-corpus goldens of the present layer read it).
#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(clippy::struct_excessive_bools)] // one flag per golden column
pub struct PresentShape {
    /// The role needs a name to be worth speaking.
    pub needs_name: bool,
    /// A focus presentation asks for the description of the node.
    pub description: bool,
    /// The focus of the role flaps (settle delay).
    pub flaps: bool,
    /// The node is the label of a control.
    pub label: bool,
    /// A label may be inferred for the control.
    pub infers_label: bool,
    /// How a dialog read takes part: `text`, `container` or `skip`.
    pub part: &'static str,
    /// The window reads its content on open.
    pub reads_content: bool,
}

/// The [`PresentShape`] of a node.
pub fn present_shape(snap: &NodeSnapshot) -> PresentShape {
    use crate::nodekind::{is_label, role_of};
    let role = role_of(snap);
    let (part, reads_content) = dialog_part(snap);
    PresentShape {
        needs_name: crate::present::needs_name(role),
        description: crate::present::focus_need(snap, false).contains(PropMask::DESCRIPTION),
        flaps: crate::focus::settle_delay(Some(role)) > std::time::Duration::ZERO,
        label: is_label(snap),
        infers_label: gorca_doc::web::label::infers_for(role),
        part,
        reads_content,
    }
}

/// A page of snapshots as a [`NavSource`], every node fresh.
#[derive(Debug)]
pub struct PageSource(pub HashMap<NodeKey, Arc<NodeSnapshot>>);

impl NavSource for PageSource {
    fn view(&self, k: NodeKey) -> Lookup<NodeView> {
        match self.0.get(&k) {
            Some(s) => Lookup::Hit(NodeView {
                snap: Arc::clone(s),
                stale: PropMask::empty(),
                aged: false,
            }),
            None => Lookup::Gone,
        }
    }
}
