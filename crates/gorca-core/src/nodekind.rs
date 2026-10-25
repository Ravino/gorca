// SPDX-License-Identifier: GPL-3.0-or-later
//! What kind of node a snapshot is, asked of its fused entry (ADR-0022 section 8.7, M22c): the
//! model role and the flags the entry carries, never the connector's role. Small questions that
//! several surfaces ask the same way live here, so they have one answer.

use gorca_model::sem::{SemFlags, SemRole};
use gorca_model::{NodeSnapshot, PropMask};

use crate::present::entry_of;

/// The model role of a snapshot (its attached entry, else the provisional one).
pub(crate) fn role_of(snap: &NodeSnapshot) -> SemRole {
    entry_of(snap).sem.role
}

/// The application root of the accessibility tree, as opposed to an ARIA `application` widget
/// (which carries an author's role token, [`SemFlags::ROLE_AUTHORED`]) or an embedded object (an
/// HTML `<object>` is exported with the same model role): the root is the one application node
/// that has no parent in its own application, which only a snapshot that carries its parent can
/// say. A snapshot that does not is not taken for the root; the walk fetches its ancestry.
pub fn is_app_root(snap: &NodeSnapshot) -> bool {
    let e = entry_of(snap);
    e.sem.role == SemRole::Application
        && !e.sem.flags.contains(SemFlags::ROLE_AUTHORED)
        && snap.have().contains(PropMask::PARENT)
        && snap.parent().is_none()
}

/// The application node of an ancestry chain, as far as the chain says: [`is_app_root`], or an
/// application node whose parent was never fetched (the chain ends there). An embedded object
/// inside the tree has a parent and is not one.
pub(crate) fn is_app_node(snap: &NodeSnapshot) -> bool {
    let e = entry_of(snap);
    e.sem.role == SemRole::Application
        && !e.sem.flags.contains(SemFlags::ROLE_AUTHORED)
        && (snap.parent().is_none() || !snap.have().contains(PropMask::PARENT))
}

/// The label of a control, as the engines export it: a `label` (static text the model marks
/// [`SemFlags::TEXT_LABEL`]) or a caption.
pub(crate) fn is_label(snap: &NodeSnapshot) -> bool {
    let e = entry_of(snap);
    e.sem.role == SemRole::Caption
        || (e.sem.role == SemRole::StaticText && e.sem.flags.contains(SemFlags::TEXT_LABEL))
}

/// A top-level window or a dialog: what "the window" of a node is.
pub(crate) fn is_window_role(role: SemRole) -> bool {
    matches!(
        role,
        SemRole::Window | SemRole::Dialog | SemRole::AlertDialog
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_and_application_nodes_are_told_by_the_entry() {
        use crate::review::collect::testsrc::snap;
        use gorca_model::Role;
        assert!(is_label(&snap(1, Role::Label, "L", None, &[])));
        assert!(!is_label(&snap(2, Role::StaticText, "S", None, &[])));
        let app = snap(3, Role::Application, "A", None, &[]);
        assert!(is_app_node(&app) && is_app_root(&app));
        // An application node inside the tree is an embedded object, not the root.
        let inner = snap(4, Role::Application, "O", Some(3), &[]);
        assert!(!is_app_node(&inner) && !is_app_root(&inner));
        assert_eq!(role_of(&app), SemRole::Application);
    }

    #[test]
    fn window_roles_are_windows_and_dialogs() {
        assert!(is_window_role(SemRole::Window) && is_window_role(SemRole::Dialog));
        assert!(is_window_role(SemRole::AlertDialog));
        assert!(!is_window_role(SemRole::Alert) && !is_window_role(SemRole::Application));
    }
}
