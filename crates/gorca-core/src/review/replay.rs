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
