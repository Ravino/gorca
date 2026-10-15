
    // ---- M3 review probe (appended inside lane.rs mod tests, then restored) ----
    /// P5: say-all's clock gate assumes the lane handed chunk 1 to the server within 150 ms.
    /// With the server down (restarting) at say-all start, the inflight epoch never advances,
    /// so chunks 2 and 3 (same epoch, sent together when the gate opens by the clock) go
    /// through the interrupting slot and replace chunk 1 / each other.
    #[tokio::test]
    async fn probe_gate_chunks_lost_when_server_down_at_start() {
        let (h, ctl, epoch, _t) = start(true, ClientHealth::Down);
        let (tx, mut rx) = gorca_rt::mpsc::channel(64);
        h.set_progress(tx);
        let e = epoch.advance();
        h.submit(utt(1, e.0, Prio::Text, "chunk 1"));
        tokio::time::sleep(Duration::from_millis(150)).await;
        h.submit(utt(2, e.0, Prio::Text, "chunk 2"));
        h.submit(utt(3, e.0, Prio::Text, "chunk 3"));
        tokio::time::sleep(Duration::from_millis(30)).await;
        ctl.set_health(ClientHealth::Ready);
        assert!(eventually(WAIT, || !speaks(&ctl).is_empty()).await);
        for _ in 0..3 {
            if let Some(&last) = speaks(&ctl).last() {
                ctl.send(SpeechEvent::Started(UttId(last)));
                ctl.send(SpeechEvent::Ended(UttId(last)));
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
        let mut got = Vec::new();
        while let Ok(p) = rx.try_recv() {
            got.push(p);
        }
        eprintln!("P5 speaks {:?} progress {:?} stats {:?}", speaks(&ctl), got, h.stats());
        assert_eq!(speaks(&ctl), [1, 2, 3], "P5: every say-all chunk reaches the server");
    }
