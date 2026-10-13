//! The whole core pipeline without a bus: scripted application -> ingress -> app actor -> Reader
//! -> fetch -> utterance. Real tasks, real threads, real time (margins are generous; the claims
//! are about ordering and bounds, not microseconds).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::disallowed_methods
)]

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use gorca_connector_api::{Connector, ConnectorCtx};
use gorca_core::{Core, CoreConfig, SpeechOut};
use gorca_model::{ConnectorId, Role, StateSet, Utterance};
use gorca_rt::{CancellationToken, Clock};
use gorca_testkit::{Behavior, ScriptedApp, ScriptedConnector, ScriptedHandle, ScriptedNode};

#[derive(Debug, Default)]
struct Recording {
    spoken: Mutex<Vec<(Instant, String)>>,
    epoch_notifies: std::sync::atomic::AtomicU64,
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

    fn notify_epoch(&self) {
        self.epoch_notifies
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

struct Rig {
    speech: Arc<Recording>,
    handle: ScriptedHandle,
    stats: Arc<gorca_core::CoreStats>,
    mirrors: Arc<gorca_core::MirrorDirectory>,
    shutdown: CancellationToken,
    core: tokio::task::JoinHandle<Result<(), gorca_core::CoreError>>,
    connector: tokio::task::JoinHandle<Result<(), gorca_connector_api::ConnectorError>>,
}

async fn rig(apps: Vec<ScriptedApp>) -> Rig {
    let speech = Arc::new(Recording::default());
    let mut core =
        Core::new(CoreConfig::default()).with_speech(Arc::clone(&speech) as Arc<dyn SpeechOut>);
    let registry = core.connector_registrar(ConnectorId(1));
    let stats = core.stats();
    let mirrors = core.mirrors();
    let shutdown = CancellationToken::new();
    let core = tokio::spawn(core.run(shutdown.clone()));
    let (conn, handle) = ScriptedConnector::new(apps.clone());
    let ctx = ConnectorCtx {
        id: ConnectorId(1),
        registry,
        shutdown: shutdown.clone(),
        clock: Clock::System,
    };
    let connector = tokio::spawn(Box::new(conn).run(ctx));
    for a in &apps {
        handle.wait_app(&a.scope).await;
    }
    Rig {
        speech,
        handle,
        stats,
        mirrors,
        shutdown,
        core,
        connector,
    }
}

impl Rig {
    async fn wait_spoken(&self, n: usize) -> Vec<String> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let t = self.speech.texts();
            if t.len() >= n || Instant::now() > deadline {
                return t;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }

    /// Wait until `text` was spoken; returns when (relative to `since`) it was recorded.
    async fn wait_for(&self, text: &str, since: Instant) -> Option<Duration> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let found = self
                .speech
                .spoken
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .iter()
                .find(|(_, t)| t == text)
                .map(|(at, _)| at.saturating_duration_since(since));
            if found.is_some() || Instant::now() > deadline {
                return found;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }

    async fn stop(self) {
        self.shutdown.cancel();
        assert_eq!(self.core.await.unwrap(), Ok(()));
        assert!(self.connector.await.unwrap().is_ok());
    }
}

fn form(scope: &str) -> ScriptedApp {
    ScriptedApp {
        scope: scope.to_owned(),
        nodes: vec![
            (1, ScriptedNode::new(Role::Button, "OK")),
            (
                2,
                ScriptedNode::new(Role::CheckBox, "Subscribe")
                    .with_states(StateSet::CHECKED | StateSet::FOCUSABLE),
            ),
            (3, ScriptedNode::new(Role::Link, "Docs")),
            (
                4,
                ScriptedNode::new(Role::Button, "Delete").with_states(StateSet::DISABLED),
            ),
        ],
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn focus_is_spoken_with_name_role_and_states() {
    let r = rig(vec![form("app")]).await;
    r.handle.focus("app", 2);
    assert_eq!(r.wait_spoken(1).await, ["Subscribe check box checked"]);
    r.handle.focus("app", 4);
    assert_eq!(r.wait_spoken(2).await[1], "Delete button unavailable");
    assert!(
        r.speech
            .epoch_notifies
            .load(std::sync::atomic::Ordering::Relaxed)
            >= 2,
        "every focus change wakes the speech lane"
    );
    r.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_focus_storm_speaks_only_the_last_focus() {
    let r = rig(vec![form("app")]).await;
    // Every answer takes 40 ms (inside the 120 ms Interactive deadline), far longer than the burst (a synchronous loop of pushes): all
    // but the last are superseded before their reply arrives (E-3).
    r.handle
        .set_behavior("app", Behavior::Delay(Duration::from_millis(40)));
    for i in 0..200u64 {
        r.handle.focus("app", 1 + (i % 3));
    }
    // 200 pushes: node ids cycle 1,2,3; the last one pushed is i = 199 -> node 1 + 199 % 3 = 2.
    let spoken = r.wait_spoken(1).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let spoken_after = r.speech.texts();
    assert_eq!(
        spoken_after.last().map(String::as_str),
        Some("Subscribe check box checked"),
        "{spoken_after:?}"
    );
    assert!(
        spoken_after.len() <= 3,
        "a storm must not become speech: {spoken_after:?} (first: {spoken:?})"
    );
    let s = r.stats.snapshot();
    assert!(s.focus_requests >= 1);
    assert!(
        s.utterances == spoken_after.len() as u64 && s.utterances <= s.fetch_ok,
        "superseded answers are dropped, not spoken: {s:?}"
    );
    r.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hung_app_never_stalls_the_reader_and_recovers() {
    let r = rig(vec![form("good"), form("bad")]).await;
    r.handle.set_behavior("bad", Behavior::Hang);

    // Three focus changes on the hung app: three Interactive timeouts make it Hung (I-4).
    for node in [1, 2, 3] {
        r.handle.focus("bad", node);
        tokio::time::sleep(Duration::from_millis(170)).await;
    }
    // While it is hung, the other app still answers within a few milliseconds.
    let t0 = Instant::now();
    r.handle.focus("good", 3);
    let latency = r
        .wait_for("Docs link", t0)
        .await
        .expect("the healthy app was heard");
    assert!(latency < Duration::from_millis(100), "{latency:?}");
    assert!(
        r.speech
            .texts()
            .contains(&"application not responding".to_owned()),
        "the hung focused app was announced"
    );
    assert_eq!(
        r.stats.snapshot().fetch_timeout,
        3,
        "exactly the three Interactive requests timed out"
    );

    // A Hung app receives no request but its probe: a new focus is refused at once.
    let refused_before = r.stats.snapshot().fetch_refused;
    r.handle.focus("bad", 1);
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert!(r.stats.snapshot().fetch_refused > refused_before);

    // It recovers when the probe succeeds (probe backoff starts at 250 ms).
    r.handle.set_behavior("bad", Behavior::Answer);
    let deadline = Instant::now() + Duration::from_secs(6);
    let mut node = 1;
    loop {
        node = node % 3 + 1;
        let before = r.speech.texts().len();
        r.handle.focus("bad", node);
        tokio::time::sleep(Duration::from_millis(200)).await;
        if r.speech.texts().len() > before {
            break;
        }
        assert!(Instant::now() < deadline, "the app never recovered");
    }
    r.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn duplicate_focus_reports_are_one_utterance() {
    let r = rig(vec![form("app")]).await;
    r.handle.focus("app", 1);
    tokio::time::sleep(Duration::from_millis(30)).await;
    // GTK3 style: `focus:` and `state-changed:focused` for the same change.
    r.handle.focus("app", 1);
    let spoken = r.wait_spoken(1).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(spoken, ["OK button"]);
    assert_eq!(r.speech.texts().len(), 1);
    r.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_vanished_app_is_cleaned_up() {
    let r = rig(vec![form("app")]).await;
    r.handle.focus("app", 1);
    r.wait_spoken(1).await;
    assert_eq!(r.mirrors.len(), 1);
    r.handle.gone("app");
    let deadline = Instant::now() + Duration::from_secs(2);
    while !r.mirrors.is_empty() {
        assert!(Instant::now() < deadline, "the app actor did not end");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    r.stop().await;
}

/// A speech sink that panics on every utterance: the Reader panics each time it speaks.
struct Exploding;

impl SpeechOut for Exploding {
    fn submit(&self, _u: Utterance) {
        panic!("speech sink exploded");
    }

    fn notify_epoch(&self) {}
}

/// Regression (review F8): when the Reader lane gives up after repeated panics, the core must
/// stop with an error so the process exits and is restarted, never stay up mute.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dead_reader_lane_stops_the_core_with_an_error() {
    let mut core = Core::new(CoreConfig {
        max_reader_restarts: 2,
        ..CoreConfig::default()
    })
    .with_speech(Arc::new(Exploding) as Arc<dyn SpeechOut>);
    let registry = core.connector_registrar(ConnectorId(1));
    let shutdown = CancellationToken::new();
    let core = tokio::spawn(core.run(shutdown.clone()));
    let (conn, handle) = ScriptedConnector::new(vec![form("app")]);
    let ctx = ConnectorCtx {
        id: ConnectorId(1),
        registry,
        shutdown: shutdown.clone(),
        clock: Clock::System,
    };
    let connector = tokio::spawn(Box::new(conn).run(ctx));
    handle.wait_app("app").await;
    // Each focus change makes the sink (and so the Reader) panic: more than 2 in the window.
    let mut node = 1;
    let deadline = Instant::now() + Duration::from_secs(10);
    while !core.is_finished() {
        node = node % 3 + 1;
        handle.focus("app", node);
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            Instant::now() < deadline,
            "the core never reported the dead lane"
        );
    }
    assert_eq!(
        core.await.unwrap(),
        Err(gorca_core::CoreError::ReaderLaneDied)
    );
    assert!(
        shutdown.is_cancelled(),
        "the rest of the process is told to stop"
    );
    let _ = connector.await;
}
