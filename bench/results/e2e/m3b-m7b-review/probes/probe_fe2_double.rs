// Append to the end of crates/gorca-core/src/text/tests.rs (FE2 #260 replay).
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
