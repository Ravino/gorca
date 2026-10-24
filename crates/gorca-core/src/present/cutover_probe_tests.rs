// SPDX-License-Identifier: GPL-3.0-or-later
//! Test-only hook of the cut-over gate (`cargo xtask cutover change`, ADR-0022 section 8.9): the
//! legacy change presenter as a function over two snapshots, so the gate can replay recorded
//! corpora through both presenters. Compiled only with the `cutover-probe` feature (enabled by
//! the connector's dev-dependencies), named `*_tests.rs` like the rest of the test code of the
//! legacy path, and deleted with `present_change` when the change surface is cut over.

use std::sync::Arc;

use gorca_model::{Epoch, NodeSnapshot, SegmentKind, StateSet, UttId};

use super::{changed, present_change};

/// What the legacy presenter would speak when `old` becomes `new`; empty when it stays silent.
/// `skip_selected` is the switcher-context rule (selection is not news there).
pub fn legacy_change_tokens(
    old: &NodeSnapshot,
    new: &NodeSnapshot,
    skip_selected: bool,
) -> Vec<(SegmentKind, Arc<str>)> {
    let skip = if skip_selected {
        StateSet::SELECTED
    } else {
        StateSet::empty()
    };
    let ch = changed(old, new, skip);
    if ch.is_empty() {
        return Vec::new();
    }
    present_change(new, ch, UttId(1), Epoch(1), None)
        .segments
        .iter()
        .map(|s| (s.kind, Arc::clone(&s.text)))
        .collect()
}
