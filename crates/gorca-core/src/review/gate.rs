// SPDX-License-Identifier: GPL-3.0-or-later
//! The replay half of the review cut-over gate needs both paths side by side: the legacy zone
//! text and classification (read off the legacy snapshot fields) and the ones made from the fused
//! entry. This module is the seam `gorca-connector-atspi`'s replay calls (`cargo xtask cutover
//! review`, ADR-0022 section 8.9). It exists only until the cut-over commit, which deletes the
//! legacy side together with the replay.

use std::collections::HashMap;
use std::sync::Arc;

use gorca_model::{Lookup, NodeKey, NodeSnapshot, NodeView, PropMask, SegmentKind};

pub use super::model::ReviewLimits;
pub use super::objnav::{
    NavMove, NavSource, PresClass, StepResult, class, class_in, class_of_snapshot, step_with,
};
pub use super::zone::{join_tokens, object_text, object_tokens, role_word, zone_tokens};

use super::cluster::{BuildNode, legacy_zone_tokens};
use crate::present::flat_zone_tokens;

/// The pieces the legacy flat-review text of a node says.
pub fn legacy_flat(snap: &NodeSnapshot) -> Vec<(SegmentKind, String)> {
    flat_zone_tokens(snap)
}

/// The pieces the legacy object zone of a node says (its role word for a silent object that can
/// be acted on or focused).
pub fn legacy_zone(snap: &Arc<NodeSnapshot>) -> Vec<(SegmentKind, String)> {
    legacy_zone_tokens(&BuildNode {
        snap: Arc::clone(snap),
        depth: 0,
        lines: None,
    })
}

/// The legacy role word of a node for a nameless object.
pub fn legacy_role_word(snap: &NodeSnapshot) -> &'static str {
    snap.role.spoken()
}

/// Whether the zone of an object is made even when it says nothing: it can be focused or acted on.
pub fn actionable(snap: &NodeSnapshot) -> bool {
    snap.struct_state()
        .contains(gorca_model::StructFlags::FOCUSABLE)
        || snap.interfaces.contains(gorca_model::Ifaces::ACTION)
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
