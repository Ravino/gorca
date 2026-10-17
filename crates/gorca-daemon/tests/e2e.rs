// SPDX-License-Identifier: GPL-3.0-or-later
//! End to end: fake AT-SPI application on a private bus -> the real daemon wiring (AT-SPI
//! connector, core, speech lane, failover client, voicego and SSIP clients) -> mock speech
//! servers. Everything runs in private, isolated sessions (`tests/harness`); the user's session,
//! buses, Orca and speech services are never touched.
//!
//! Needs the harness prerequisites (see `Session::unavailable_reason`); without them each test
//! prints `SKIPPED` and passes.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::disallowed_methods
)]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use gorca_daemon::{Arm, Options, SpeechMode, run_with};
use gorca_rt::CancellationToken;
use gorca_testkit::MockSpeechServer;
use gorca_testkit::harness::{FakeApp, Session};

fn session() -> Option<Session> {
    if let Some(why) = Session::unavailable_reason() {
        eprintln!("SKIPPED (harness unavailable): {why}");
        return None;
    }
    Some(Session::start().expect("isolated session"))
}

/// A directory for a mock socket, inside the session's temporary directory.
fn sock_dir(session: &Session, name: &str) -> PathBuf {
    let d = session.dir().join("run").join(name);
    std::fs::create_dir_all(&d).unwrap();
    d
}

struct Daemon {
    token: CancellationToken,
    join: tokio::task::JoinHandle<anyhow::Result<()>>,
    /// Cancels the daemon when it is dropped without `stop` (a failing test).
    _guard: tokio_util::sync::DropGuard,
}

impl Daemon {
    fn start(
        session: &Session,
        speech: SpeechMode,
        voicego: &std::path::Path,
        ssip: &std::path::Path,
    ) -> Self {
        Self::start_in(session, speech, voicego, ssip, Arm::default())
    }

    fn start_in(
        session: &Session,
        speech: SpeechMode,
        voicego: &std::path::Path,
        ssip: &std::path::Path,
        topology: Arm,
    ) -> Self {
        let opts = Options {
            speech,
            topology,
            ..Options::isolated(
                session.a11y_address(),
                session.session_address(),
                voicego,
                ssip,
            )
        };
        let token = CancellationToken::new();
        let join = tokio::spawn(run_with(opts, token.clone()));
        Self {
            _guard: token.clone().drop_guard(),
            token,
            join,
        }
    }

    async fn stop(self) {
        self.token.cancel();
        let r = tokio::time::timeout(Duration::from_secs(10), self.join).await;
        assert!(
            matches!(r, Ok(Ok(Ok(())))),
            "daemon did not stop cleanly: {r:?}"
        );
    }
}

fn run<F: std::future::Future<Output = ()>>(f: F) {
    gorca_testkit::harness::run_bounded(4, std::time::Duration::from_secs(300), f);
}

async fn wait_text(server: &MockSpeechServer, text: &str, timeout: Duration) -> Option<Duration> {
    let t0 = Instant::now();
    while t0.elapsed() < timeout {
        if server.spoken().iter().any(|t| t.trim() == text) {
            return Some(t0.elapsed());
        }
        tokio::time::sleep(Duration::from_millis(3)).await;
    }
    None
}

/// Like [`wait_text`], but only over what was spoken after the first `mark` utterances.
async fn wait_text_since(
    server: &MockSpeechServer,
    mark: usize,
    text: &str,
    timeout: Duration,
) -> bool {
    let t0 = Instant::now();
    while t0.elapsed() < timeout {
        if server.spoken().iter().skip(mark).any(|t| t.trim() == text) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(3)).await;
    }
    false
}

/// Wait until the daemon is really up (speech connected, drain subscribed, app announced):
/// focus `node` until `server` has heard something. Events sent before the drain's match rules
/// are installed are lost by design, so a fixed sleep would be a race on a loaded machine.
async fn prime(app: &mut FakeApp, server: &MockSpeechServer, node: &str) {
    let deadline = Instant::now() + Duration::from_secs(25);
    let mut flip = false;
    loop {
        // The app only emits an event when focus really moves, so alternate.
        flip = !flip;
        app.focus(if flip { node } else { "Cancel" }).unwrap();
        if server.wait_spoken(1, Duration::from_millis(400)).await {
            return;
        }
        assert!(Instant::now() < deadline, "the daemon never became ready");
    }
}

/// B-5: with the speech lane on its own thread and runtime, focus still reaches the speech
/// server, and the daemon still stops cleanly (the lane threads are joined).
#[test]
fn the_lanes_topology_speaks_and_stops_cleanly() {
    let Some(session) = session() else { return };
    run(async {
        let voicego = MockSpeechServer::start_voicego(&sock_dir(&session, "v"))
            .await
            .unwrap();
        let ssip_path = sock_dir(&session, "s").join("none.sock");
        let mut app = FakeApp::start(&session, &["--scenario", "form"]).unwrap();
        let d = Daemon::start_in(
            &session,
            SpeechMode::Voicego,
            voicego.socket_path(),
            &ssip_path,
            Arm::Lanes,
        );
        prime(&mut app, &voicego, "Submit").await;
        app.focus("Subscribe").unwrap();
        assert!(
            wait_text(
                &voicego,
                "Subscribe, check box, not checked",
                Duration::from_secs(5)
            )
            .await
            .is_some(),
            "got {:?}",
            voicego.spoken()
        );
        d.stop().await;
    });
}

#[test]
fn a_fake_app_moving_focus_reaches_voicego_as_the_right_utterances() {
    let Some(session) = session() else { return };
    run(async {
        let voicego = MockSpeechServer::start_voicego(&sock_dir(&session, "v"))
            .await
            .unwrap();
        let ssip_path = sock_dir(&session, "s").join("none.sock");
        let mut app = FakeApp::start(&session, &["--scenario", "form"]).unwrap();
        let d = Daemon::start(
            &session,
            SpeechMode::Voicego,
            voicego.socket_path(),
            &ssip_path,
        );
        prime(&mut app, &voicego, "name").await;

        for (node, said) in [
            ("Subscribe", "Subscribe, check box, not checked"),
            ("Submit", "Submit, button"),
        ] {
            app.focus(node).unwrap();
            let waited = wait_text(&voicego, said, Duration::from_secs(5)).await;
            assert!(
                waited.is_some(),
                "`{said}` not spoken; got {:?}",
                voicego.spoken()
            );
        }
        // Toggle the checkbox in the app; re-entering it announces the new state.
        app.cmd("\"cmd\":\"action\",\"node\":\"Subscribe\",\"index\":0")
            .unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        app.focus("Cancel").unwrap();
        app.focus("Subscribe").unwrap();
        assert!(
            wait_text(
                &voicego,
                "Subscribe, check box, checked",
                Duration::from_secs(5)
            )
            .await
            .is_some(),
            "{:?}",
            voicego.spoken()
        );
        // Interrupts go out as stop+speak in one write; the server saw STOPs before SPEAKs.
        let verbs = voicego.verbs();
        assert!(verbs.iter().any(|v| v.starts_with("STOP")), "{verbs:?}");
        drop(app);
        d.stop().await;
    });
}

#[test]
fn a_focus_storm_ends_with_only_the_last_focus_audible() {
    let Some(session) = session() else { return };
    run(async {
        let voicego = MockSpeechServer::start_voicego(&sock_dir(&session, "v"))
            .await
            .unwrap();
        // Utterances take a while to play, like real speech: each new focus must cut the old.
        voicego.set_speak_time(Duration::from_millis(400));
        let ssip_path = sock_dir(&session, "s").join("none.sock");
        let mut app = FakeApp::start(&session, &["--scenario", "form"]).unwrap();
        let d = Daemon::start(
            &session,
            SpeechMode::Voicego,
            voicego.socket_path(),
            &ssip_path,
        );
        prime(&mut app, &voicego, "name").await;
        // Let the priming utterance end so it is not mistaken for storm output.
        tokio::time::sleep(Duration::from_millis(500)).await;

        let nodes = ["name", "email", "Subscribe", "Submit"];
        let n = 300;
        for i in 0..n {
            app.focus(nodes[i % nodes.len()]).unwrap();
        }
        app.focus("Cancel").unwrap();
        assert!(
            wait_text(&voicego, "Cancel, button", Duration::from_secs(5))
                .await
                .is_some(),
            "{:?}",
            voicego.spoken()
        );
        tokio::time::sleep(Duration::from_millis(600)).await;
        let spoken = voicego.spoken();
        assert_eq!(
            spoken.last().map(|s| s.trim()),
            Some("Cancel, button"),
            "the last focus is the last thing said: {:?}",
            &spoken[spoken.len().saturating_sub(5)..]
        );
        assert!(
            spoken.len() < n / 2,
            "{} utterances for {n} focus events: events were not coalesced or superseded",
            spoken.len()
        );
        drop(app);
        d.stop().await;
    });
}

#[test]
fn a_hung_app_leaves_the_reader_responsive() {
    let Some(session) = session() else { return };
    run(async {
        let voicego = MockSpeechServer::start_voicego(&sock_dir(&session, "v"))
            .await
            .unwrap();
        let ssip_path = sock_dir(&session, "s").join("none.sock");
        let mut good = FakeApp::start(&session, &["--scenario", "form", "--name", "good"]).unwrap();
        let mut bad = FakeApp::start(&session, &["--scenario", "form", "--name", "bad"]).unwrap();
        let d = Daemon::start(
            &session,
            SpeechMode::Voicego,
            voicego.socket_path(),
            &ssip_path,
        );
        prime(&mut bad, &voicego, "name").await;
        bad.hang("never", 0).unwrap();
        for node in ["email", "Submit", "Subscribe"] {
            bad.focus(node).unwrap();
            tokio::time::sleep(Duration::from_millis(170)).await;
        }
        let t0 = Instant::now();
        good.focus("Submit").unwrap();
        let heard = wait_text(&voicego, "Submit, button", Duration::from_secs(5)).await;
        assert!(heard.is_some(), "{:?}", voicego.spoken());
        assert!(
            t0.elapsed() < Duration::from_millis(500),
            "{:?}",
            t0.elapsed()
        );
        assert!(
            voicego
                .spoken()
                .iter()
                .any(|t| t.trim() == "application not responding"),
            "{:?}",
            voicego.spoken()
        );
        drop(good);
        drop(bad);
        d.stop().await;
    });
}

#[test]
fn voicego_down_falls_back_to_ssip_and_returns_when_it_is_back() {
    let Some(session) = session() else { return };
    run(async {
        let mut voicego = MockSpeechServer::start_voicego(&sock_dir(&session, "v"))
            .await
            .unwrap();
        let ssip = MockSpeechServer::start_ssip(&sock_dir(&session, "s"))
            .await
            .unwrap();
        // voicego is down from the start.
        voicego.stop().await;
        let mut app = FakeApp::start(&session, &["--scenario", "form"]).unwrap();
        let d = Daemon::start(
            &session,
            SpeechMode::Auto,
            voicego.socket_path(),
            ssip.socket_path(),
        );
        prime(&mut app, &ssip, "email").await;
        // The failover notice alone can satisfy `prime`; move focus explicitly.
        app.focus("Cancel").unwrap();
        app.focus("email").unwrap();
        assert!(
            wait_text(&ssip, "email, edit", Duration::from_secs(10))
                .await
                .is_some(),
            "ssip got {:?}",
            ssip.spoken()
        );
        assert!(
            wait_text(
                &ssip,
                "voicego unavailable, using speech-dispatcher",
                Duration::from_secs(5)
            )
            .await
            .is_some(),
            "the user is told once: {:?}",
            ssip.spoken()
        );
        assert!(voicego.spoken().is_empty());

        // voicego comes back: the daemon notices by probing and moves speech back to it.
        voicego.restart().unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut node = 0;
        let names = ["name", "email", "Submit", "Cancel"];
        loop {
            node = (node + 1) % names.len();
            app.focus(names[node]).unwrap();
            if voicego.wait_spoken(1, Duration::from_millis(300)).await {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "speech never returned to voicego"
            );
        }

        // voicego dies again mid-session: the next focus still gets spoken, by ssip.
        let before = ssip.spoken().len();
        voicego.stop().await;
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            node = (node + 1) % names.len();
            app.focus(names[node]).unwrap();
            if ssip
                .wait_spoken(before + 1, Duration::from_millis(300))
                .await
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "never fell back after voicego died"
            );
        }
        drop(app);
        d.stop().await;
    });
}

#[test]
fn speech_ssip_mode_never_touches_voicego() {
    let Some(session) = session() else { return };
    run(async {
        let voicego = MockSpeechServer::start_voicego(&sock_dir(&session, "v"))
            .await
            .unwrap();
        let ssip = MockSpeechServer::start_ssip(&sock_dir(&session, "s"))
            .await
            .unwrap();
        let mut app = FakeApp::start(&session, &["--scenario", "form"]).unwrap();
        let d = Daemon::start(
            &session,
            SpeechMode::Ssip,
            voicego.socket_path(),
            ssip.socket_path(),
        );
        prime(&mut app, &ssip, "Submit").await;
        app.focus("Submit").unwrap();
        assert!(
            wait_text(&ssip, "Submit, button", Duration::from_secs(5))
                .await
                .is_some()
        );
        assert_eq!(voicego.accepted(), 0);
        drop(app);
        d.stop().await;
    });
}

#[test]
fn key_presses_are_echoed_through_the_orca_keyboard_monitor_name() {
    let Some(session) = session() else { return };
    run(async {
        let voicego = MockSpeechServer::start_voicego(&sock_dir(&session, "v"))
            .await
            .unwrap();
        let ssip_path = sock_dir(&session, "s").join("none.sock");
        let mut mutter = FakeApp::start_mutter(&session).unwrap();
        let opts = Options {
            speech: SpeechMode::Voicego,
            impersonate_orca: true,
            no_keyboard: false,
            ..Options::isolated(
                session.a11y_address(),
                session.session_address(),
                voicego.socket_path(),
                &ssip_path,
            )
        };
        let token = CancellationToken::new();
        let join = tokio::spawn(run_with(opts, token.clone()));

        // `a` (keycode 38, keysym 0x61); the keyboard source retries until the manager accepts
        // it as the owner of org.gnome.Orca.KeyboardMonitor.
        let deadline = Instant::now() + Duration::from_secs(20);
        while !voicego.wait_spoken(1, Duration::from_millis(300)).await {
            mutter.key(38, 0x61, 0x61).unwrap();
            assert!(
                Instant::now() < deadline,
                "no key echo: {:?}",
                voicego.spoken()
            );
        }
        assert_eq!(voicego.spoken()[0].trim(), "a");
        // The mock's stats prove it was the unicast signal to the name owner that arrived.
        let stats = mutter.cmd("\"cmd\":\"stats\"").unwrap();
        assert!(stats.contains("\"denied\":0"), "{stats}");

        token.cancel();
        let r = tokio::time::timeout(Duration::from_secs(10), join).await;
        assert!(matches!(r, Ok(Ok(Ok(())))), "{r:?}");
    });
}

const SWITCHER: [&str; 6] = [
    "Terminal",
    "Files",
    "Firefox",
    "Settings",
    "Calculator",
    "Text Editor",
];

/// Select items of the fake switcher alternately until the daemon speaks one: the profile of
/// the application and the drain's subscriptions are then both in place.
async fn prime_switcher(app: &mut FakeApp, server: &MockSpeechServer) {
    let deadline = Instant::now() + Duration::from_secs(25);
    let mark = server.spoken().len();
    let mut flip = false;
    loop {
        flip = !flip;
        app.select(if flip { SWITCHER[0] } else { SWITCHER[1] })
            .unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        if server
            .spoken()
            .iter()
            .skip(mark)
            .any(|t| t.trim().ends_with(", button"))
        {
            return;
        }
        assert!(Instant::now() < deadline, "the daemon never became ready");
    }
}

/// GNOME Shell's Alt+Tab switcher only moves `state-changed:selected`: every move must be
/// spoken, not just the first couple (the live-test bug), including coming back to an item.
#[test]
fn every_selection_move_in_a_shell_style_switcher_is_spoken() {
    let Some(session) = session() else { return };
    run(async {
        let voicego = MockSpeechServer::start_voicego(&sock_dir(&session, "v"))
            .await
            .unwrap();
        let ssip_path = sock_dir(&session, "s").join("none.sock");
        let mut app = FakeApp::start(
            &session,
            &[
                "--scenario",
                "switcher",
                "--name",
                "gnome-shell",
                "--own-shell-name",
            ],
        )
        .unwrap();
        let d = Daemon::start(
            &session,
            SpeechMode::Voicego,
            voicego.socket_path(),
            &ssip_path,
        );
        prime_switcher(&mut app, &voicego).await;

        // Two laps over the list, at a pace faster than a person can press Tab.
        let mut order: Vec<&str> = SWITCHER.to_vec();
        order.extend(SWITCHER);
        for name in order {
            // The last priming selection may still be on this item; step off it first.
            app.select(if name == SWITCHER[1] {
                SWITCHER[2]
            } else {
                SWITCHER[1]
            })
            .unwrap();
            tokio::time::sleep(Duration::from_millis(170)).await;
            let mark = voicego.spoken().len();
            app.select(name).unwrap();
            let said = format!("{name}, button");
            assert!(
                wait_text_since(&voicego, mark, &said, Duration::from_secs(5)).await,
                "`{said}` not spoken; got {:?}",
                &voicego.spoken()[mark..]
            );
        }
        assert!(
            voicego.spoken().iter().all(|t| !t.contains("selected")),
            "being selected is not announced: {:?}",
            voicego.spoken()
        );
        drop(app);
        d.stop().await;
    });
}

/// Alt+Tab: the Tab presses are not echoed (a held Alt makes it a command), and the item the
/// switcher moves to is spoken after every press.
#[test]
fn alt_tab_in_the_switcher_speaks_windows_and_never_echoes_tab() {
    let Some(session) = session() else { return };
    run(async {
        let voicego = MockSpeechServer::start_voicego(&sock_dir(&session, "v"))
            .await
            .unwrap();
        let ssip_path = sock_dir(&session, "s").join("none.sock");
        let mut mutter = FakeApp::start_mutter(&session).unwrap();
        let mut app = FakeApp::start(
            &session,
            &[
                "--scenario",
                "switcher",
                "--name",
                "gnome-shell",
                "--own-shell-name",
            ],
        )
        .unwrap();
        let opts = Options {
            speech: SpeechMode::Voicego,
            impersonate_orca: true,
            no_keyboard: false,
            ..Options::isolated(
                session.a11y_address(),
                session.session_address(),
                voicego.socket_path(),
                &ssip_path,
            )
        };
        let token = CancellationToken::new();
        let join = tokio::spawn(run_with(opts, token.clone()));

        // Both the keyboard source and the switcher profile must be live.
        let deadline = Instant::now() + Duration::from_secs(25);
        while wait_text(&voicego, "a", Duration::from_millis(300))
            .await
            .is_none()
        {
            mutter.key(38, 0x61, 0x61).unwrap();
            assert!(Instant::now() < deadline, "no key echo");
        }
        prime_switcher(&mut app, &voicego).await;
        // Park the selection on an item the loop below does not start with.
        app.select(SWITCHER[5]).unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        let mark = voicego.spoken().len();

        // Alt is held (modifier mask 8); each Tab moves the selection a bit later, the way the
        // key reaches Shell after gorca saw it. Shift+Alt+Tab goes backwards.
        for (i, name) in SWITCHER.iter().enumerate() {
            let state = if i % 2 == 0 { 8 } else { 8 | 1 };
            mutter.key_with_state(23, 0xff09, 0, state).unwrap();
            tokio::time::sleep(Duration::from_millis(30)).await;
            app.select(name).unwrap();
            let said = format!("{name}, button");
            assert!(
                wait_text_since(&voicego, mark, &said, Duration::from_secs(5)).await,
                "`{said}` not spoken; got {:?}",
                &voicego.spoken()[mark..]
            );
            tokio::time::sleep(Duration::from_millis(170)).await;
        }
        let after: Vec<String> = voicego.spoken()[mark..].to_vec();
        assert!(
            after.iter().all(|t| t.trim() != "tab"),
            "Tab under Alt must not be echoed: {after:?}"
        );
        // A bare Tab is a key name: silent while `echo.command_keys` is off (the default).
        mutter.key(23, 0xff09, 0).unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(
            voicego.spoken()[mark..].iter().all(|t| t.trim() != "tab"),
            "{:?}",
            &voicego.spoken()[mark..]
        );

        drop(app);
        token.cancel();
        let r = tokio::time::timeout(Duration::from_secs(10), join).await;
        assert!(matches!(r, Ok(Ok(Ok(())))), "{r:?}");
    });
}
