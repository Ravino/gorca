// Reviewer probes for M8b browse presentation. Insert as `mod probe_m8b { ... }` at the end of
// `mod tests` in crates/gorca-core/src/browse/present.rs (needs `use super::*;`).
use super::*;

fn blk2() -> NodeFlags {
    NodeFlags::BLOCK | NodeFlags::BREAK_BEFORE
}

// P1: a check box named only by aria-label (no visible label). NVDA renders the name
// (gecko_ia2.cpp nameIsContent for check/radio when the label is not visible).
#[test]
fn probe_checkbox_aria_label_in_line() {
    let cb = SubtreeSpec::leaf(
        aria_node(
            Role::CheckBox,
            StateSet::ENABLED | StateSet::FOCUSABLE,
            &[(web_attrs::EXPLICIT_NAME, "true"), (web_attrs::TAG, "input")],
            None,
        )
        .name("Subscribe box")
        .flags(blk2())
        .text("\u{FFFC}"),
    );
    let d = doc_of(vec![cb]);
    eprintln!("P1 line: {}", line_of(&d, 1));
    assert!(line_of(&d, 1).contains("Subscribe box"), "{}", line_of(&d, 1));
}

// P2: a heading with aria-label. NVDA: heading is nameIsContent, the label replaces content.
#[test]
fn probe_heading_aria_label() {
    let h = SubtreeSpec::with_children(
        aria_node(
            Role::Heading,
            StateSet::ENABLED,
            &[(web_attrs::EXPLICIT_NAME, "true"), (web_attrs::TAG, "h2"), (web_attrs::LEVEL, "2")],
            None,
        )
        .name("Short heading label")
        .flags(blk2()),
        vec![txt("Long visible heading content")],
    );
    let d = doc_of(vec![h]);
    eprintln!("P2 line: {}", line_of(&d, 1));
    assert!(line_of(&d, 1).contains("Short heading label"), "{}", line_of(&d, 1));
}

// P4: a tree item with aria-label. NVDA's nameIsContent list has no outline item: content stays.
#[test]
fn probe_tree_item_aria_label() {
    let ti = SubtreeSpec::with_children(
        aria_node(
            Role::TreeItem,
            StateSet::ENABLED,
            &[(web_attrs::EXPLICIT_NAME, "true"), (web_attrs::TAG, "li"), (web_attrs::XML_ROLES, "treeitem")],
            None,
        )
        .name("Item label")
        .flags(blk2()),
        vec![txt("Item content")],
    );
    let d = doc_of(vec![ti]);
    eprintln!("P4 line: {}", line_of(&d, 1));
}

// P5: page text that starts with the PAUSE prefix. With description_pause=false the strip
// removes it from content too.
#[test]
fn probe_pause_prefix_collides_with_content() {
    let p = SubtreeSpec::with_children(leaf(Role::Paragraph).flags(blk2()), vec![txt(". Dot leading paragraph")]);
    let d = doc_of(vec![p]);
    let s = d.line_start(1).unwrap();
    let (_, e) = d.line_range(s);
    let segs = segments(&describe(&d, s, e));
    let mut u = gorca_model::Utterance {
        id: gorca_model::UttId(1),
        segments: segs,
        lang: None,
        epoch: gorca_model::Epoch(1),
        prio: gorca_model::Prio::Interrupt,
        may_refine: false,
        survives_focus_change: false,
        ttl_secs: 0,
        refines: None,
    };
    eprintln!("P5 before: {:?}", u.segments.iter().map(|s| s.text.to_string()).collect::<Vec<_>>());
    crate::aria::strip_pause(&mut u);
    eprintln!("P5 after strip: {:?}", u.segments.iter().map(|s| s.text.to_string()).collect::<Vec<_>>());
    assert!(u.segments.iter().any(|s| s.text.starts_with(". Dot")), "content altered");
}
