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

use gorca_daemon::{Options, SpeechMode, run_with};
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
}

impl Daemon {
    fn start(
        session: &Session,
        speech: SpeechMode,
        voicego: &std::path::Path,
        ssip: &std::path::Path,
    ) -> Self {
        let opts = Options {
            speech,
            no_keyboard: true,
            voicego_socket: Some(voicego.to_owned()),
            ssip_socket: Some(ssip.to_owned()),
            a11y_address: Some(session.a11y_address().to_owned()),
            session_address: Some(session.session_address().to_owned()),
            ..Options::default()
        };
        let token = CancellationToken::new();
        let join = tokio::spawn(run_with(opts, token.clone()));
        Self { token, join }
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
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap()
        .block_on(f);
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
            ("Subscribe", "Subscribe, check box"),
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
