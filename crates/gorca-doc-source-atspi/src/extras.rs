//! M8b: what the browsers say about a node beyond role, state, name and attributes, and the
//! per-engine rows that normalise it.
//!
//! * [`WebQuirks`] is the engine quirk table of this source (ADR-0008): data, one row per
//!   behaviour, chosen once from the application's identity. Everything downstream (the core
//!   presenters) reads the normalised result and never asks which engine it is.
//! * [`error_text`] reads the `aria-errormessage` target of an invalid node: the engines expose
//!   it only as a relation (`error-message`), never in `Description`.

use std::sync::Arc;

use gorca_doc_source_api::limits;
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
    /// Row `gecko-list-marker-in-name`: Gecko's name of a list item or tree item that has no
    /// author-given name is its text with the list marker in front (`• Documents`, `1. First`).
    /// The marker is drawn by CSS, not content: the source drops it from the name.
    pub strip_list_marker: bool,
}

impl WebQuirks {
    pub const GECKO: WebQuirks = WebQuirks {
        read_only_when_not_editable: true,
        strip_list_marker: true,
    };
    pub const BLINK: WebQuirks = WebQuirks {
        read_only_when_not_editable: false,
        strip_list_marker: false,
    };
    /// No engine rows (toolkits the source does not know).
    pub const NONE: WebQuirks = WebQuirks {
        read_only_when_not_editable: false,
        strip_list_marker: false,
    };

    /// Normalise the name of a node (`authored`: the author gave the name, so it is kept).
    pub fn name(self, role: Role, authored: bool, name: Option<Arc<str>>) -> Option<Arc<str>> {
        if !self.strip_list_marker || authored || !matches!(role, Role::ListItem | Role::TreeItem) {
            return name;
        }
        let n = name?;
        let t = without_list_marker(&n);
        if t.len() == n.len() {
            Some(n)
        } else if t.is_empty() {
            None
        } else {
            Some(Arc::from(t))
        }
    }

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

/// `s` without a leading list marker: a bullet and a space, or digits, a dot and a space.
pub fn without_list_marker(s: &str) -> &str {
    for m in [
        "\u{2022} ",
        "\u{25e6} ",
        "\u{25aa} ",
        "\u{2023} ",
        "\u{25a0} ",
        "\u{25a1} ",
        "\u{2013} ",
    ] {
        if let Some(r) = s.strip_prefix(m) {
            return r.trim_start();
        }
    }
    let digits = s.bytes().take_while(u8::is_ascii_digit).count();
    if digits > 0
        && let Some(r) = s[digits..].strip_prefix(". ")
    {
        return r.trim_start();
    }
    s
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

/// Most characters requested from an error message target (a page can point `aria-errormessage`
/// at a whole article).
const ERROR_TEXT_CHARS: i32 = 400;

/// The text of the `aria-errormessage` target of `path`, if it has one: the relation target's
/// `Text`, else its name. `None` when there is no such relation or no text.
pub async fn error_text(conn: &AppConn, path: &str, deadline: Deadline) -> Option<Arc<str>> {
    let lane = conn.next_lane();
    // Gecko activates its relations cache domain on the first query and answers that one with an
    // empty set: ask again once after a moment.
    let mut target = None;
    for attempt in 0..2 {
        let m = conn
            .call(lane, deadline, path, ACCESSIBLE, "GetRelationSet", &())
            .await
            .ok()?;
        let rels = wire::parse_relations(m.body().data().bytes()).ok()?;
        target = rels
            .into_iter()
            .find(|(t, _)| *t == RELATION_ERROR_MESSAGE)
            .and_then(|(_, ts)| ts.into_iter().next())
            .map(|(_, p)| p);
        if target.is_some() || attempt == 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    }
    let target = target.as_deref()?;
    // The end offset is bounded in the request (and never past the end of the text, which Gecko
    // refuses), so a huge target cannot make a huge reply.
    let count = match conn
        .call(
            lane + 1,
            deadline,
            target,
            PROPERTIES,
            "Get",
            &(TEXT, "CharacterCount"),
        )
        .await
    {
        Ok(m) => wire::parse_variant_i32(m.body().data().bytes()).unwrap_or(0),
        Err(_) => 0,
    };
    let end = count.clamp(0, ERROR_TEXT_CHARS);
    let text = match conn
        .call(lane + 1, deadline, target, TEXT, "GetText", &(0i32, end))
        .await
    {
        Ok(m) => wire::parse_string(m.body().data().bytes())
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty()),
        Err(_) => None,
    };
    let text = if let Some(t) = text {
        t
    } else {
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
            .filter(|s| !s.is_empty())?
    };
    let text = text.replace('\u{FFFC}', "").replace('\n', " ");
    let text = limits::clip_str(text.trim(), limits::MAX_DESCRIPTION_BYTES);
    (!text.is_empty()).then(|| Arc::from(text))
}

/// `error_text` for many nodes, concurrently, inside the caller's deadline (the fetch's own:
/// the extra reads never outlive it). Results are in input order. The caller bounds how many
/// paths it passes; each node costs at most four calls.
pub async fn error_texts(
    conn: &Arc<AppConn>,
    paths: Vec<String>,
    deadline: Deadline,
) -> Vec<Option<Arc<str>>> {
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
        .map(|s| limits::clip_str(s, limits::MAX_DESCRIPTION_BYTES))
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
    fn gecko_drops_the_list_marker_from_an_unnamed_item_only() {
        let g = WebQuirks::GECKO;
        let n = |s: &str| Some(Arc::<str>::from(s));
        assert_eq!(
            g.name(Role::TreeItem, false, n("\u{2022} Documents"))
                .as_deref(),
            Some("Documents")
        );
        assert_eq!(
            g.name(Role::ListItem, false, n("12. Twelfth")).as_deref(),
            Some("Twelfth")
        );
        // An authored name is kept, and so is a name that only starts with a number.
        assert_eq!(
            g.name(Role::ListItem, true, n("\u{2022} Documents"))
                .as_deref(),
            Some("\u{2022} Documents")
        );
        assert_eq!(
            g.name(Role::ListItem, false, n("2024 plan")).as_deref(),
            Some("2024 plan")
        );
        assert_eq!(
            g.name(Role::Link, false, n("\u{2022} Documents"))
                .as_deref(),
            Some("\u{2022} Documents")
        );
        assert_eq!(
            WebQuirks::BLINK
                .name(Role::TreeItem, false, n("1. A"))
                .as_deref(),
            Some("1. A")
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
