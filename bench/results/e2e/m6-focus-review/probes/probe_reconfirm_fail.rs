// Insert at the end of `mod tests` in crates/gorca-core/src/reader.rs as
// `mod probe_reconfirm { use super::*; <this file> }`.
// A reconfirm read that fails (Timeout / Busy under load) is never retried: the application
// stays unprotected for the whole focus, so its text events (typed echo) are discarded, and
// since 78fa165 no key restores it.
#[test]
fn probe_a_failed_reconfirm_read_is_never_retried() {
    let mut f = fixture();
    let published = Arc::new(FocusedApp::new());
    f.reader.focused_app = Some(Arc::clone(&published));
    let now = Instant::now();
    focus_on(&mut f, 1, now);
    f.reader.handle(
        reply(&f, 1, FetchKind::Focus, snap(1, Role::Button, "Term")),
        now,
    );
    let fx = focus_on(&mut f, 1, now + Duration::from_millis(8));
    assert_eq!(effects_of(&fx), ["fetch-s1"]);
    let fx = f.reader.handle(
        ReaderMsg::Reply(QueryReply {
            node: key(1),
            epoch: f.epoch.current(),
            kind: FetchKind::StateChange,
            result: Err(gorca_connector_api::BackendError::Timeout),
        }),
        now,
    );
    assert!(effects_of(&fx).is_empty(), "no retry after a failed read");
    let fx = focus_on(&mut f, 1, now + Duration::from_millis(40));
    assert!(effects_of(&fx).is_empty(), "a later repeated event asks nothing either");
    assert_eq!(published.get(), None, "the focused terminal stays unprotected");
}
