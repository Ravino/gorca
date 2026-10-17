//! M8b: what the browsers say about a node beyond role, state, name and attributes, and the
//! per-engine rows that normalise it.
//!
//! * [`WebQuirks`] is the engine quirk table of this source (ADR-0008): data, one row per
//!   behaviour, chosen once from the application's identity. Everything downstream (the core
//!   presenters) reads the normalised result and never asks which engine it is.
//! * [`error_text`] reads the `aria-errormessage` target of an invalid node: the engines expose
//!   it only as a relation (`error-message`), never in `Description`.

use std::sync::Arc;
use std::time::Instant;

use gorca_model::{Role, StateSet};
use gorca_rt::Deadline;

use crate::p2p::AppConn;
use crate::wire;

const ACCESSIBLE: &str = "org.a11y.atspi.Accessible";
const TEXT: &str = "org.a11y.atspi.Text";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";

/// AT-SPI relation type of `aria-errormessage` (`ATSPI_RELATION_ERROR_MESSAGE`).
const RELATION_ERROR_MESSAGE: u32 = 21;

/// Quirk rows of the web engines, applied while the source builds a node.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct WebQuirks {
    /// Row `gecko-readonly-absent-editable`: Gecko has no `read-only` state. A text field that
    /// can take focus but lacks `editable` is a read-only one (`<input readonly>`); the source
    /// adds `READ_ONLY` so that the presenters need no engine knowledge. Blink sets `read-only`
    /// itself, for `readonly` and `aria-readonly` alike.
    pub read_only_when_not_editable: bool,
}

impl WebQuirks {
    pub const GECKO: WebQuirks = WebQuirks {
        read_only_when_not_editable: true,
    };
    pub const BLINK: WebQuirks = WebQuirks {
        read_only_when_not_editable: false,
    };
    /// No engine rows (toolkits the source does not know).
    pub const NONE: WebQuirks = WebQuirks {
        read_only_when_not_editable: false,
    };

    /// Normalise the states of a node.
    pub fn states(self, role: Role, mut states: StateSet) -> StateSet {
        if self.read_only_when_not_editable
            && role == Role::TextEntry
            && states.contains(StateSet::FOCUSABLE)
            && !states.contains(StateSet::EDITABLE)
        {
            states |= StateSet::READ_ONLY;
        }
        states
    }
}

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

/// Longest error message text kept (a page can point `aria-errormessage` at a whole article).
const ERROR_TEXT_CAP: i32 = 400;

/// The text of the `aria-errormessage` target of `path`, if it has one: the relation target's
/// `Text`, else its name. `None` when there is no such relation or no text.
pub async fn error_text(conn: &AppConn, path: &str, deadline: Deadline) -> Option<Arc<str>> {
    let lane = conn.next_lane();
    let m = conn
        .call(lane, deadline, path, ACCESSIBLE, "GetRelationSet", &())
        .await
        .ok()?;
    let rels = wire::parse_relations(m.body().data().bytes()).ok()?;
    let (_, targets) = rels.iter().find(|(t, _)| *t == RELATION_ERROR_MESSAGE)?;
    let (_, target) = targets.first()?;
    let text = match conn
        .call(
            lane + 1,
            deadline,
            target,
            TEXT,
            "GetText",
            &(0i32, ERROR_TEXT_CAP),
        )
        .await
    {
        Ok(m) => wire::parse_string(m.body().data().bytes())
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty()),
        Err(_) => None,
    };
    let text = match text {
        Some(t) => Some(t),
        None => {
            let m = conn
                .call(
                    lane + 2,
                    deadline,
                    target,
                    PROPERTIES,
                    "Get",
                    &(ACCESSIBLE, "Name"),
                )
                .await
                .ok()?;
            wire::parse_variant_string(m.body().data().bytes())
                .ok()
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
        }
    }?;
    let text = text.replace('\u{FFFC}', "").replace('\n', " ");
    let text = text.trim();
    (!text.is_empty()).then(|| Arc::from(text))
}

/// `error_text` for many nodes, concurrently, within one deadline. Results are in input order.
pub async fn error_texts(
    conn: &Arc<AppConn>,
    paths: Vec<String>,
    budget: std::time::Duration,
) -> Vec<Option<Arc<str>>> {
    let deadline = Deadline(Instant::now() + budget);
    let mut set = tokio::task::JoinSet::new();
    for (i, p) in paths.into_iter().enumerate() {
        let conn = Arc::clone(conn);
        set.spawn(async move { (i, error_text(&conn, &p, deadline).await) });
    }
    let mut out: Vec<Option<Arc<str>>> = vec![None; set.len()];
    while let Some(r) = set.join_next().await {
        if let Ok((i, t)) = r
            && let Some(slot) = out.get_mut(i)
        {
            *slot = t;
        }
    }
    out
}

/// `Properties.Get(Accessible, "Description")`, empty = `None`.
pub async fn description(conn: &AppConn, path: &str, deadline: Deadline) -> Option<Arc<str>> {
    let m = conn
        .call(
            conn.next_lane(),
            deadline,
            path,
            PROPERTIES,
            "Get",
            &(ACCESSIBLE, "Description"),
        )
        .await
        .ok()?;
    wire::parse_variant_string(m.body().data().bytes())
        .ok()
        .filter(|s| !s.is_empty())
        .map(Arc::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gecko_read_only_is_a_focusable_entry_that_is_not_editable() {
        let g = WebQuirks::GECKO;
        let focusable = StateSet::FOCUSABLE | StateSet::ENABLED;
        assert!(
            g.states(Role::TextEntry, focusable)
                .contains(StateSet::READ_ONLY)
        );
        assert!(
            !g.states(Role::TextEntry, focusable | StateSet::EDITABLE)
                .contains(StateSet::READ_ONLY)
        );
        // A disabled field is not focusable and is not called read only.
        assert!(
            !g.states(Role::TextEntry, StateSet::EDITABLE)
                .contains(StateSet::READ_ONLY)
        );
        assert!(
            !g.states(Role::Link, focusable)
                .contains(StateSet::READ_ONLY)
        );
        assert!(
            !WebQuirks::BLINK
                .states(Role::TextEntry, focusable)
                .contains(StateSet::READ_ONLY)
        );
    }

    #[test]
    fn only_controls_take_an_error_text() {
        assert!(takes_error_text(Role::TextEntry));
        assert!(takes_error_text(Role::CheckBox));
        assert!(!takes_error_text(Role::Landmark));
        assert!(!takes_error_text(Role::Panel));
    }
}
