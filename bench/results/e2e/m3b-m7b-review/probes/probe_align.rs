// Append inside `mod align_tests` of crates/gorca-bench/src/e2e/webtext.rs; run
// `cargo test -p gorca-bench --lib probe_align`; restore with git checkout.
    /// Key 0 says nothing; key 1 (same text) is spoken twice, 0.3 ms apart, right after key 1.
    /// The latency closure mirrors `score_aligned(from_event=true)`: from the last page caret
    /// event between the key and the speech (key 1's event at 140.2 ms).
    #[test]
    fn probe_align_silent_key_then_double_is_hidden() {
        let pr = [p(0, "x"), p(140, "x")];
        let sp = [(140_500_000u64, "x".to_owned()), (140_800_000, "x".to_owned())];
        let events = [200_000u64, 140_200_000];
        let sc = align(&pr, &sp, |i, t| {
            let t0 = events
                .iter()
                .copied()
                .filter(|e| *e >= pr[i].t_key && *e <= t)
                .max()
                .unwrap_or(pr[i].t_key);
            (t - t0) as f64 / 1e6
        });
        // A correct scorer reports one lost key and one double.
        assert_eq!((sc.lost, sc.doubled, sc.extra), (1, 1, 0), "hidden: {sc:?}");
    }
