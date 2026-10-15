// Append to the end of crates/gorca-core/src/text/tests.rs; run
// `cargo test -p gorca-core --lib probe_m3b`; restore with git checkout.
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
