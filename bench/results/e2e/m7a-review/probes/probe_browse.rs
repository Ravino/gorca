// M7a review probe: appended as `mod probe_m7a_browse` at the end of browse/tests.rs.
use super::*;

/// A focused field the mirror and the join do not know yet (cold focus, Lookup::Miss): after
/// LOADING_GRACE the timer puts the user in browse mode with the browse grabs on, while the
/// focus is still in the field.
#[test]
fn probe_cold_field_becomes_browse_mode_after_the_grace() {
    let mut r = rig();
    r.focus(3000); // not in the desktop mirror, not in the join
    assert!(matches!(r.mode(), WebMode::Loading { editable: false, .. }));
    r.reader.handle(
        ReaderMsg::Doc(DocNotice::InDocument { focus: nk(3000), doc: DocId(1) }),
        r.now,
    );
    // The focus snapshot arrives: it is an edit field. Nothing re-evaluates `editable`.
    r.mirror.apply(
        SnapshotBatch {
            nodes: vec![snap(3000, Role::TextEntry, "Search", StateSet::EDITABLE | StateSet::FOCUSABLE)],
            ..Default::default()
        },
        ApplyOrigin::Push,
    );
    r.now += Duration::from_millis(1100);
    r.reader.handle(ReaderMsg::Timer, r.now);
    eprintln!("web mode {:?}, reader mode {:?}", r.mode(), r.reader.mode());
    assert!(matches!(r.mode(), WebMode::Browse { .. }));
    assert!(matches!(r.reader.mode(), Mode::Browse { .. }), "browse grabs are on");
}

fn page2_root_only() -> Vec<DocDelta> {
    let mut body = block(src(1, Role::DocumentWeb, Some(5001)));
    body.name = Some(Arc::from("Second"));
    vec![
        DocDelta::Reset { root: SrcNodeId(1), nav_id: 1 },
        DocDelta::Upsert(vec![body]),
        DocDelta::Settled { batch: 1 },
    ]
}

fn page2_rest() -> Vec<DocDelta> {
    vec![
        DocDelta::Upsert(vec![
            block(src(2, Role::Paragraph, Some(5002))),
            run(3, "First line of the second page"),
        ]),
        kids(1, &[2]),
        kids(2, &[3]),
        DocDelta::Settled { batch: 2 },
    ]
}

/// Enter on a link: the new document's first publish holds the root only (progressive build or
/// a slow fetch). Focus on its root comes while the old document is still in browse mode.
#[test]
fn probe_navigation_loses_the_first_line() {
    let mut r = rig();
    r.focus(1006);
    r.key(CoreCmd::BrowseCursorNextLine);
    r.key(CoreCmd::BrowseCursorNextLine);
    // Doc 2 appears with its root only.
    let (mut w2, read2) = channel(9);
    let mut t2 = Translator::new();
    let out = t2.apply(&mut w2, page2_root_only()).unwrap();
    w2.publish();
    for (l, id) in out.join_add {
        r.join.set(NodeKey { app: app(), local: l }, (DocId(2), id));
    }
    let (ctl_tx, _ctl2) = mpsc::channel(16);
    let (ev_tx, _ev2) = mpsc::channel(16);
    r.docs.insert(DocHandle {
        doc: DocId(2),
        app: app(),
        root: nk(5001),
        read: read2,
        ctl: ctl_tx,
        events: ev_tx,
    });
    let fx_pub = r.reader.handle(
        ReaderMsg::Doc(DocNotice::FirstUseful { doc: DocId(2), app: app() }),
        r.now,
    );
    r.mirror.apply(
        SnapshotBatch {
            nodes: vec![snap(5001, Role::DocumentWeb, "Second", StateSet::FOCUSABLE)],
            ..Default::default()
        },
        ApplyOrigin::Push,
    );
    let fx_focus = r.focus(5001);
    let out = t2.apply(&mut w2, page2_rest()).unwrap();
    w2.publish();
    for (l, id) in out.join_add {
        r.join.set(NodeKey { app: app(), local: l }, (DocId(2), id));
    }
    let fx_changed = r.reader.handle(
        ReaderMsg::Doc(DocNotice::Changed { doc: DocId(2), rev: 2 }),
        r.now,
    );
    let fx_down = r.key(CoreCmd::BrowseCursorNextLine);
    eprintln!(
        "first-useful: {:?}\nfocus: {:?}\nchanged: {:?}\ndown: {:?}\nmode {:?}",
        spoken(&fx_pub),
        spoken(&fx_focus),
        spoken(&fx_changed),
        spoken(&fx_down),
        r.mode()
    );
    let all: Vec<String> = [spoken(&fx_pub), spoken(&fx_focus), spoken(&fx_changed)].concat();
    assert!(
        !all.iter().any(|s| s.contains("First line")),
        "the first line was spoken: {all:?}"
    );
}

/// A lost `Gone` notice (the Reader's doc queue was full): the mode stays Browse on a document
/// the directory no longer has, and every browse key is grabbed and silently does nothing.
#[test]
fn probe_browse_on_a_vanished_document_swallows_keys_silently() {
    let mut r = rig();
    r.focus(1006);
    r.docs.remove(DocId(1));
    let fx = r.key(CoreCmd::QuicknavNextHeading);
    eprintln!("mode {:?}; spoken {:?}", r.reader.mode(), spoken(&fx));
    assert!(matches!(r.reader.mode(), Mode::Browse { .. }));
    assert!(spoken(&fx).is_empty());
}
