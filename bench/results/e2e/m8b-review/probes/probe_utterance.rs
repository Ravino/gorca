// Reviewer probes for M8b focus speech. Insert as `mod probe_m8b { ... }` at the end of
// `mod change_tests` in crates/gorca-core/src/present/utterance.rs.
use super::*;

// P6: a Firefox chrome / XUL menu item (Gecko gives chrome elements a `tag` too) that is
// selectable and focusable but not selected. NVDA never says "not selected" for MENUITEM.
#[test]
fn probe_menu_item_not_selected() {
    let mut n = snap("Bookmarks", StateSet::SELECTABLE | StateSet::FOCUSABLE, None);
    n.role = Role::MenuItem;
    n.attrs = vec![(gorca_doc_source_api::web_attrs::TAG, Arc::<str>::from("menuitem"))].into_iter().collect();
    let t = texts(&present_focus(&n, UttId(1), Epoch(1)));
    eprintln!("P6: {t:?}");
    assert!(!t.contains(&"not selected".to_owned()), "{t:?}");
}

// P7: NVDA state order: HASPOPUP_DIALOG (bit 47) after REQUIRED (15) and INVALID (17);
// ON (46) after REQUIRED.
#[test]
fn probe_state_order() {
    let mut n = snap("Pick", StateSet::REQUIRED | StateSet::HAS_POPUP | StateSet::COLLAPSED, None);
    n.role = Role::Button;
    n.attrs = vec![
        (gorca_doc_source_api::web_attrs::TAG, Arc::<str>::from("button")),
        (gorca_doc_source_api::web_attrs::HASPOPUP, Arc::<str>::from("dialog")),
    ].into_iter().collect();
    let t = texts(&present_focus(&n, UttId(1), Epoch(1)));
    eprintln!("P7: {t:?}");
}
