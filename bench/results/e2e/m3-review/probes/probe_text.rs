
// ---- M3 review probes (appended to crates/gorca-core/src/text/tests.rs, then restored) ----

/// P1: selection-changed arrives before text-caret-moved for a Shift+Right.
#[test]
fn probe_selection_event_before_caret_event() {
    let mut g = rig();
    g.entry("hello", 0);
    let _ = g.key(RIGHT, 0, Modifiers::SHIFT);
    let f1 = fetches(&g.selection_event());
    assert_eq!(f1.len(), 1, "selection fetch issued");
    let fx_c = g.caret(1);
    let f2 = fetches(&fx_c);
    let fx = g.reply(
        f1[0].0,
        Reply::Selections(TextSelections {
            ranges: vec![(0..1, Arc::from("h"))],
        }),
    );
    let mut all = spoken(&fx_c);
    all.extend(spoken(&fx));
    all.extend(spoken(&g.tick(10)));
    eprintln!("P1 second fetches {f2:?} spoken {all:?}");
    assert_eq!(all, ["h selected"], "P1: selection speech for Shift+Right");
}

/// P2: a lost Ended report keeps say-all Running past the end; the next key then moves the
/// caret back to the start of the last chunk.
#[test]
fn probe_say_all_lost_progress_report() {
    let mut g = rig();
    let text = "One. Two.";
    let (_, f) = say_all_start(&mut g, text);
    let fx = g.reply(
        f[0].0,
        Reply::Window(TextWindowFill {
            base: 0,
            text: Arc::from(text),
            len: 9,
            caret: None,
        }),
    );
    let mut ids = utt_ids(&fx);
    let fx = g.msg(ReaderMsg::Text(TextMsg::Progress(
        gorca_speech::LaneProgress::Started(ids[0]),
    )));
    ids.extend(utt_ids(&fx));
    // Ended(ids[0]) is lost (text_io full); the second chunk starts and ends normally.
    let _ = g.msg(ReaderMsg::Text(TextMsg::Progress(
        gorca_speech::LaneProgress::Started(ids[1]),
    )));
    let fx_end = g.msg(ReaderMsg::Text(TextMsg::Progress(
        gorca_speech::LaneProgress::Ended(ids[1]),
    )));
    let natural = fetches(&fx_end);
    eprintln!("P2 at end: fetches {natural:?}");
    // A minute later the user presses Down.
    g.advance(60_000);
    let fx = g.key(DOWN, 0, NONE);
    let f = fetches(&fx);
    eprintln!("P2 after Down: fetches {f:?}");
    assert!(
        !f.iter().any(|(_, op)| matches!(op, TextOp::SetCaret { .. })),
        "P2: a key long after the reading ended must not move the caret"
    );
}

/// P3: typing a key whose insert never arrives (no text-changed), then an uncorrelated caret
/// move of one character within the expectation TTL.
#[test]
fn probe_unrelated_caret_move_absorbed_by_stale_expectations() {
    let mut g = rig();
    g.entry("hello world", 0);
    let _ = g.typed('x');
    g.advance(300);
    // A mouse click one character to the right, no key.
    let mut all = spoken(&g.caret(1));
    all.extend(spoken(&g.tick(10)));
    eprintln!("P3 spoken {all:?}");
    assert!(!all.is_empty(), "P3: an uncorrelated caret move is spoken");
}

/// P4: caret beats key2 while the slot for key1 (no movement at a boundary) is still open.
#[test]
fn probe_caret_beats_key_while_previous_slot_open() {
    let mut g = rig();
    g.entry("ab cd", 5);
    let _ = g.key(RIGHT, 0, NONE); // at the end: no caret event will come
    g.advance(20);
    let mut all = spoken(&g.caret(4)); // Left's caret beats the Left key
    let _ = g.key(LEFT, 0, NONE);
    for _ in 0..50 {
        all.extend(spoken(&g.tick(10)));
    }
    eprintln!("P4 spoken {all:?}");
    assert_eq!(all.len(), 1, "P4: one utterance for the Left key");
}
