// SPDX-License-Identifier: GPL-3.0-or-later
//! The AT-SPI connector against a real (private) a11y bus and fake applications, with the real
//! core behind it and a recording speech sink. Needs `tests/harness` prerequisites; when they
//! are missing the tests say so and pass (they are listed in the final report as not run).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::disallowed_methods
)]

use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::{Duration, Instant};

use gorca_connector_api::{Connector, ConnectorCtx};
use gorca_connector_atspi::{AtspiConnector, AtspiOptions};
use gorca_core::{Core, CoreConfig, SpeechOut};
use gorca_model::{ConnectorId, Utterance};
use gorca_rt::{CancellationToken, Clock};
use gorca_testkit::harness::{FakeApp, Session};

/// The CPU-time test needs the process to itself; every other test shares the read side.
static EXCLUSIVE: RwLock<()> = RwLock::new(());

#[derive(Debug, Default)]
struct Recording {
    spoken: Mutex<Vec<(Instant, String)>>,
}

impl Recording {
    fn texts(&self) -> Vec<String> {
        self.spoken
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(_, t)| t.clone())
            .collect()
    }
}

impl SpeechOut for Recording {
    fn submit(&self, u: Utterance) {
        let text = u
            .segments
            .iter()
            .map(|s| &*s.text)
            .collect::<Vec<_>>()
            .join(" ");
        self.spoken
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((Instant::now(), text));
    }

    fn notify_epoch(&self) {}
}

struct Rig {
    session: Option<Session>,
    speech: Arc<Recording>,
    shutdown: CancellationToken,
    core: tokio::task::JoinHandle<Result<(), gorca_core::CoreError>>,
    connector: tokio::task::JoinHandle<Result<(), gorca_connector_api::ConnectorError>>,
}

fn session() -> Option<Session> {
    // `GORCA_LOG=debug cargo test ... -- --nocapture` shows what the connector and core do.
    if let Ok(f) = tracing_subscriber::EnvFilter::try_from_env("GORCA_LOG") {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(f)
            .with_writer(std::io::stderr)
            .try_init();
    }
    if let Some(why) = Session::unavailable_reason() {
        eprintln!("SKIPPED (harness unavailable): {why}");
        return None;
    }
    Some(Session::start().expect("isolated session"))
}

#[allow(clippy::unused_async)]
async fn rig(session: Session) -> Rig {
    let speech = Arc::new(Recording::default());
    let mut core =
        Core::new(CoreConfig::default()).with_speech(Arc::clone(&speech) as Arc<dyn SpeechOut>);
    let registry = core.connector_registrar(ConnectorId(1));
    let shutdown = CancellationToken::new();
    let core = tokio::spawn(core.run(shutdown.clone()));
    let connector = Box::new(AtspiConnector::new(AtspiOptions {
        a11y_address: Some(session.a11y_address().to_owned()),
        session_address: Some(session.session_address().to_owned()),
        ..AtspiOptions::default()
    }));
    let ctx = ConnectorCtx {
        id: ConnectorId(1),
        registry,
        shutdown: shutdown.clone(),
        clock: Clock::System,
    };
    let connector = tokio::spawn(connector.run(ctx));
    Rig {
        session: Some(session),
        speech,
        shutdown,
        core,
        connector,
    }
}

impl Rig {
    /// Wait until the pipeline is really up: focus `node` until something was spoken. Events
    /// sent before the drain's match rules are installed are lost by design, so a fixed sleep
    /// would be a race on a loaded machine.
    async fn prime(&self, app: &mut FakeApp, node: &str) {
        let deadline = Instant::now() + Duration::from_secs(25);
        let mut flip = false;
        while self.speech.texts().is_empty() {
            // The app only emits an event when focus really moves, so alternate.
            flip = !flip;
            app.focus(if flip { node } else { "Cancel" }).unwrap();
            tokio::time::sleep(Duration::from_millis(300)).await;
            assert!(Instant::now() < deadline, "the pipeline never became ready");
        }
        // Leave focus on a known node.
        app.focus("Cancel").unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    async fn wait_for(&self, text: &str) -> Option<Duration> {
        let t0 = Instant::now();
        let deadline = t0 + Duration::from_secs(5);
        while Instant::now() < deadline {
            if self.speech.texts().iter().any(|t| t == text) {
                return Some(t0.elapsed());
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        None
    }

    async fn stop(mut self) {
        self.shutdown.cancel();
        let core = tokio::time::timeout(Duration::from_secs(20), &mut self.core).await;
        assert_eq!(core.expect("core did not stop").unwrap(), Ok(()));
        let conn = tokio::time::timeout(Duration::from_secs(20), &mut self.connector).await;
        assert!(conn.expect("connector did not stop").unwrap().is_ok());
    }
}

impl Drop for Rig {
    /// On a failing test the rig is dropped while unwinding: stop the core and the connector
    /// *before* the field drop tears the private buses down (a connector that outlives its bus
    /// must not be what keeps the test binary alive).
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

fn run<F: std::future::Future<Output = ()>>(f: F) {
    gorca_testkit::harness::run_bounded(4, Duration::from_secs(240), f);
}

#[test]
fn tabbing_through_a_form_is_heard() {
    let _shared = EXCLUSIVE.read().unwrap_or_else(PoisonError::into_inner);
    let Some(session) = session() else { return };
    run(async {
        let mut app = FakeApp::start(&session, &["--scenario", "form"]).unwrap();
        let r = rig(session).await;
        r.prime(&mut app, "email").await;
        for (node, said) in [
            ("email", "email edit"),
            ("Subscribe", "Subscribe check box not checked"),
            ("Submit", "Submit button"),
        ] {
            app.focus(node).unwrap();
            let waited = r.wait_for(said).await;
            assert!(
                waited.is_some(),
                "`{said}` was not spoken; got {:?}",
                r.speech.texts()
            );
        }
        // Focusing the checkbox again after toggling it announces the new state.
        app.cmd("\"cmd\":\"action\",\"node\":\"Subscribe\",\"index\":0")
            .unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        app.focus("Cancel").unwrap();
        app.focus("Subscribe").unwrap();
        assert!(
            r.wait_for("Subscribe check box checked").await.is_some(),
            "{:?}",
            r.speech.texts()
        );
        drop(app);
        r.stop().await;
    });
}

#[test]
fn a_hung_app_does_not_stop_the_reader() {
    let _shared = EXCLUSIVE.read().unwrap_or_else(PoisonError::into_inner);
    let Some(session) = session() else { return };
    run(async {
        let mut good = FakeApp::start(&session, &["--scenario", "form", "--name", "good"]).unwrap();
        let mut bad = FakeApp::start(&session, &["--scenario", "form", "--name", "bad"]).unwrap();
        let r = rig(session).await;
        // Both announce themselves with one focus each while healthy.
        r.prime(&mut bad, "name").await;
        // Now `bad` stops answering every method call.
        bad.hang("never", 0).unwrap();
        for node in ["email", "Submit", "Subscribe"] {
            bad.focus(node).unwrap();
            tokio::time::sleep(Duration::from_millis(170)).await;
        }
        let t0 = Instant::now();
        good.focus("Submit").unwrap();
        let waited = r.wait_for("Submit button").await;
        assert!(waited.is_some(), "{:?}", r.speech.texts());
        assert!(
            t0.elapsed() < Duration::from_millis(500),
            "healthy app answered in {:?}",
            t0.elapsed()
        );
        assert!(
            r.speech
                .texts()
                .iter()
                .any(|t| t == "application not responding"),
            "{:?}",
            r.speech.texts()
        );
        drop(good);
        drop(bad);
        r.stop().await;
    });
}

#[test]
fn an_app_that_quits_is_retired_and_a_new_one_is_heard() {
    let _shared = EXCLUSIVE.read().unwrap_or_else(PoisonError::into_inner);
    let Some(session) = session() else { return };
    run(async {
        let mut first = FakeApp::start(&session, &["--scenario", "form", "--name", "one"]).unwrap();
        let r = rig(session).await;
        r.prime(&mut first, "Submit").await;
        first.focus("Submit").unwrap();
        assert!(r.wait_for("Submit button").await.is_some());
        drop(first);
        tokio::time::sleep(Duration::from_millis(300)).await;
        let mut second = FakeApp::start(
            r.session.as_ref().unwrap(),
            &["--scenario", "form", "--name", "two"],
        )
        .unwrap();
        second.focus("Cancel").unwrap();
        assert!(r.wait_for("Cancel button").await.is_some());
        drop(second);
        r.stop().await;
    });
}

/// Process CPU time (user + system) in clock ticks, from `/proc/self/stat`.
fn cpu_ticks() -> u64 {
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap();
    let rest = &stat[stat.rfind(')').unwrap() + 2..];
    let f: Vec<&str> = rest.split_whitespace().collect();
    f[11].parse::<u64>().unwrap() + f[12].parse::<u64>().unwrap()
}

/// Regression: when the buses vanish under a running connector, it must go quiet, not spin.
/// (`watch_shell` kept a closed `NameOwnerChanged` stream in its `select!`, which completed
/// instantly on every turn: a busy loop sending messages, and a runtime that never shut down.)
#[test]
fn losing_the_buses_does_not_make_the_connector_spin() {
    let _alone = EXCLUSIVE.write().unwrap_or_else(PoisonError::into_inner);
    let Some(session) = session() else { return };
    run(async {
        let mut app = FakeApp::start(&session, &["--scenario", "form"]).unwrap();
        let mut r = rig(session).await;
        r.prime(&mut app, "email").await;
        drop(app);
        drop(r.session.take());
        let hz = 100;
        let (c0, t0) = (cpu_ticks(), Instant::now());
        tokio::time::sleep(Duration::from_secs(4)).await;
        let used = f64::from(u32::try_from(cpu_ticks() - c0).unwrap()) / f64::from(hz);
        let wall = t0.elapsed().as_secs_f64();
        // A spinning worker burns about one core (>= 0.9 of the wall time); an idle connector
        // a few percent. 0.5 separates them even on a starved host.
        assert!(
            used < 0.5 * wall,
            "{used:.2} s of CPU in {wall:.2} s after the buses went"
        );
        r.shutdown.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(20), &mut r.core).await;
        let _ = tokio::time::timeout(Duration::from_secs(20), &mut r.connector).await;
    });
}
