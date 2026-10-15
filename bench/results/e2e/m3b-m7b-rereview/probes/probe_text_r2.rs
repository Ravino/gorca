mod probe_fe2 {
    use super::*;

    fn run(gap_ms: u64, gecko: bool) -> Vec<String> {
        let mut g = if gecko { gecko_rig() } else { rig() };
        g.entry("abcdef", 6);
        g.advance(1000);
        let _ = g.key(0xff50, 0, NONE);
        let _ = g.caret(0);
        g.advance(300);
        // BackSpace at 0: the page does nothing, no event.
        let mut all = spoken(&g.key(BACKSPACE, 0, NONE));
        g.advance(gap_ms);
        all.extend(spoken(&g.tick(0)));
        all.extend(spoken(&g.typed(' ')));
        g.advance(2);
        all.extend(spoken(&g.insert(0, " ")));
        g.advance(1);
        all.extend(spoken(&g.caret(1)));
        all.extend(spoken(&g.tick(30)));
        all.extend(spoken(&g.tick(500)));
        all
    }

    #[test]
    fn probe_backspace_at_start_then_space_is_spoken_once() {
        for gap in [100u64, 500, 1000] {
            for gecko in [false, true] {
                let s = run(gap, gecko);
                assert_eq!(s, ["space"], "gap {gap} gecko {gecko}: {s:?}");
            }
        }
    }
}
mod probe_m3b {
    use super::*;

    /// A script updates an unrelated text object of the same page (a clock) while the user is
    /// in a contenteditable: the machine must stay on the paragraph and say nothing.
    #[test]
    fn probe_unrelated_text_change_does_not_steal_the_rich_editable() {
        let mut g = gecko_rig();
        let p = leaf(96);
        g.rich(p, "One two");
        g.advance(1000);
        let clock = leaf(500); // not in the mirror: Lookup::Miss
        let fx = g.insert_on(clock, 0, 5, "12:01");
        let fx2 = g.tick(50);
        let tracked = g.r.text.caret.tracked().map(|t| t.node);
        assert_eq!(
            (spoken(&fx), spoken(&fx2), tracked),
            (Vec::<String>::new(), Vec::<String>::new(), Some(p)),
            "clock insert retargeted or spoken"
        );
    }

    /// A scripted insert 1 unit after the caret (no key), then a mouse click puts the caret
    /// right there within 50 ms: the click's move must still be spoken.
    #[test]
    fn probe_scripted_insert_after_caret_swallows_a_real_move() {
        let mut g = rig();
        g.entry("hello", 2);
        g.advance(1000);
        let _ = g.insert(3, "X");
        let _ = g.tick(5);
        g.advance(10);
        let fx = g.caret(3);
        let fx2 = g.tick(20);
        let mut s = spoken(&fx);
        s.extend(spoken(&fx2));
        assert!(!s.is_empty(), "uncorrelated move to 3 was swallowed");
    }

    /// Baseline for the probe above: the same move without the scripted insert is spoken.
    #[test]
    fn probe_baseline_uncorrelated_move_is_spoken() {
        let mut g = rig();
        g.entry("helXlo", 2);
        g.advance(1000);
        let fx = g.caret(3);
        let fx2 = g.tick(20);
        let mut s = spoken(&fx);
        s.extend(spoken(&fx2));
        assert!(!s.is_empty(), "baseline move not spoken");
    }
}

mod probe_r2 {
    use super::*;
    const RIGHT: u32 = 0xff53;

    thread_local!(static PEND: std::cell::RefCell<Vec<(TextReqId, TextOp)>> = const { std::cell::RefCell::new(Vec::new()) });
    fn all(v: &mut Vec<String>, fx: ReaderEffects) -> usize {
        v.extend(spoken(&fx));
        PEND.with(|p| p.borrow_mut().extend(fetches(&fx)));
        fetches(&fx).len()
    }

    /// Gecko, typed key refused by the page while a script appends the same char far after the
    /// caret (no caret event: the caret did not move). Then the user presses Right within 50 ms.
    fn far_insert_then_right(gecko: bool, key: bool) -> (Vec<String>, usize) {
        let mut g = if gecko { gecko_rig() } else { rig() };
        g.entry("hello world", 5);
        g.advance(1000);
        let mut v = Vec::new();
        let mut f = 0;
        f += { let fx = g.typed('a'); all(&mut v, fx) };
        g.advance(3);
        let fx = g.insert(11, "a");
        f += all(&mut v, fx);
        g.advance(20);
        if key {
            let fx = g.key(RIGHT, 0, NONE);
            f += all(&mut v, fx);
        }
        g.advance(2);
        let fx = g.caret(6);
        f += all(&mut v, fx);
        let fx = g.tick(60);
        f += all(&mut v, fx.clone());
        // Answer every pending fetch with the page's real text: "hello worlda", caret 6.
        let mut pend: Vec<_> = PEND.with(|p| std::mem::take(&mut *p.borrow_mut()));
        for _ in 0..4 {
            let Some((req, op)) = pend.pop() else { break };
            let r = match op {
                TextOp::Window { .. } => Reply::Window(TextWindowFill { base: 0, text: Arc::from("hello worlda"), len: 12, caret: Some(6) }),
                _ => Reply::Unit(TextUnitAt { range: 6..7, text: Arc::from("w") }),
            };
            let fx = g.reply(req, r);
            v.extend(spoken(&fx));
            pend.extend(fetches(&fx));
        }
        v.extend(spoken(&g.tick(60)));
        (v, f)
    }

    #[test]
    fn probe_r2_far_echoed_insert_then_right_gecko() {
        let r: Vec<_> = [(true, true), (false, true), (true, false), (false, false)]
            .iter().map(|&(g, k)| ((g, k), far_insert_then_right(g, k).0)).collect();
        panic!("(gecko,key) -> spoken: {r:?}");
    }

    /// GTK entry (no quirk): a typed key the field refused (maxlength), then 2 s later a script
    /// inserts the same character: is the script's insert spoken?
    #[test]
    fn probe_r2_stale_expectation_eats_scripted_insert_2s_later() {
        let mut g = rig();
        g.entry("abc", 3);
        g.advance(1000);
        let mut v = spoken(&g.typed('e'));
        g.advance(2000);
        v.extend(spoken(&g.insert(0, "e")));
        v.extend(spoken(&g.caret(4)));
        v.extend(spoken(&g.tick(400)));
        assert_eq!(v, ["e", "e"], "the scripted insert 2 s later must be spoken");
    }

    /// Same, 16 refused keys then a script inserts a substring of them (skip matching).
    #[test]
    fn probe_r2_skip_match_eats_scripted_substring() {
        let mut g = rig();
        g.entry("abc", 3);
        g.advance(1000);
        let mut v = Vec::new();
        for c in "hello".chars() {
            v.extend(spoken(&g.typed(c)));
            g.advance(100);
        }
        v.clear();
        g.advance(500);
        v.extend(spoken(&g.insert(0, "ll")));
        v.extend(spoken(&g.tick(400)));
        assert_eq!(v, ["ll"], "scripted insert after refused keys must be spoken");
    }

    /// Paste into a contenteditable that creates a new paragraph (not mirrored yet).
    #[test]
    fn probe_r2_paste_into_new_paragraph_is_spoken() {
        let mut g = gecko_rig();
        let p = leaf(96);
        g.rich(p, "One two");
        g.advance(1000);
        let v_code = static_keycode("v").unwrap_or(0);
        let mut v = spoken(&g.key_code(v_code, 0x76, 0, Modifiers::CONTROL));
        v.extend(spoken(&g.insert_on(node(), 1, 1, "\u{FFFC}")));
        let q = leaf(98);
        v.extend(spoken(&g.insert_on(q, 0, 12, "pasted words")));
        v.extend(spoken(&g.caret_on(q, 12)));
        v.extend(spoken(&g.tick(400)));
        assert_eq!(v, ["pasted words"], "paste into a new paragraph");
    }

    /// Typing the first char into a just-created paragraph whose insert precedes its caret event.
    #[test]
    fn probe_r2_typed_into_new_paragraph_before_caret_event() {
        let mut g = gecko_rig();
        let p = leaf(96);
        g.rich(p, "One two");
        g.advance(1000);
        let q = leaf(99);
        let mut v = spoken(&g.typed('x'));
        g.advance(2);
        v.extend(spoken(&g.insert_on(q, 0, 1, "x")));
        v.extend(spoken(&g.caret_on(q, 1)));
        v.extend(spoken(&g.tick(400)));
        assert_eq!(v, ["x"]);
    }
}
