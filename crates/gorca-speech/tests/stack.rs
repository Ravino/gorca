//! The whole speech stack over real sockets: lane -> failover -> voicego / SSIP clients ->
//! mock servers. Covers cancel + speak, a focus storm, voicego down (SSIP fallback) and
//! voicego back.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gorca_model::{Epoch, Prio, Segment, SegmentKind, UttId, Utterance};
use gorca_rt::{CancellationToken, EpochCell, TaskSet, mpsc};
use gorca_speech::{
    BackendConnectError, FailoverClient, FailoverConfig, FailoverNotice, FnBackend, LaneConfig,
    SpeechBackend, SpeechHandle, SpeechLane,
};
use gorca_speech_ssip::{SsipClient, SsipOptions};
use gorca_speech_voicego::{VoicegoClient, VoicegoOptions};
use gorca_testkit::MockSpeechServer;
use smallvec::smallvec;

const WAIT: Duration = Duration::from_secs(5);

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("gst-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn utt(id: u64, epoch: Epoch, text: &str) -> Utterance {
    Utterance {
        id: UttId(id),
        segments: smallvec![Segment {
            text: Arc::from(text),
            kind: SegmentKind::Text
        }],
        lang: None,
        epoch,
        prio: Prio::Interrupt,
        may_refine: false,
        survives_focus_change: false,
        refines: None,
    }
}

fn voicego_backend(path: PathBuf) -> Arc<dyn SpeechBackend> {
    Arc::new(FnBackend::new("voicego", move |deadline| {
        let opts = VoicegoOptions::with_path(path.clone());
        async move {
            VoicegoClient::connect(&opts, deadline)
                .await
                .map(|c| Box::new(c) as Box<dyn gorca_output_api::SpeechClient>)
                .map_err(|e| BackendConnectError::new("voicego", e.to_string()))
        }
    }))
}

fn ssip_backend(path: PathBuf) -> Arc<dyn SpeechBackend> {
    Arc::new(FnBackend::new("ssip", move |deadline| {
        let opts = SsipOptions::with_path(path.clone());
        async move {
            SsipClient::connect(&opts, deadline)
                .await
                .map(|c| Box::new(c) as Box<dyn gorca_output_api::SpeechClient>)
                .map_err(|e| BackendConnectError::new("ssip", e.to_string()))
        }
    }))
}

struct Rig {
    handle: SpeechHandle,
    epoch: Arc<EpochCell>,
    notices: mpsc::Receiver<FailoverNotice>,
    _tasks: TaskSet,
}

fn rig(backends: Vec<Arc<dyn SpeechBackend>>) -> Rig {
    let mut tasks = TaskSet::new(CancellationToken::new());
    let cfg = FailoverConfig {
        connect_deadline: Duration::from_secs(1),
        probe_cap: Duration::from_millis(100),
        health_poll: Duration::from_millis(5),
        ..FailoverConfig::default()
    };
    let (client, notices) = FailoverClient::spawn(&mut tasks, backends, cfg);
    let epoch = Arc::new(EpochCell::new());
    let lane_cfg = LaneConfig {
        down_poll: Duration::from_millis(5),
        idle_poll: Duration::from_millis(20),
        ..LaneConfig::default()
    };
    let handle = SpeechLane::spawn(&mut tasks, Arc::clone(&epoch), Box::new(client), lane_cfg);
    Rig {
        handle,
        epoch,
        notices,
        _tasks: tasks,
    }
}

impl Rig {
    /// What the Reader does on focus: advance the epoch, then submit the utterance.
    fn focus(&self, id: u64, text: &str) -> Instant {
        let e = self.epoch.advance();
        let t = Instant::now();
        self.handle.submit(utt(id, e, text));
        t
    }
}

#[tokio::test]
async fn focus_is_spoken_through_voicego() {
    let d = tmp("a");
    let voicego = MockSpeechServer::start_voicego(&d).await.unwrap();
    let d2 = tmp("a2");
    let ssip = MockSpeechServer::start_ssip(&d2).await.unwrap();
    let r = rig(vec![
        voicego_backend(voicego.socket_path().to_owned()),
        ssip_backend(ssip.socket_path().to_owned()),
    ]);
    // The first utterance may arrive before the client is up: the lane holds it.
    r.focus(1, "OK button");
    assert!(voicego.wait_spoken(1, WAIT).await);
    assert_eq!(voicego.spoken(), ["OK button"]);
    assert!(ssip.spoken().is_empty());
}

#[tokio::test]
async fn a_focus_storm_over_the_wire_speaks_the_last_focus() {
    let d = tmp("b");
    let voicego = MockSpeechServer::start_voicego(&d).await.unwrap();
    voicego.set_speak_time(Duration::from_secs(5));
    let r = rig(vec![voicego_backend(voicego.socket_path().to_owned())]);
    r.focus(1, "first");
    assert!(voicego.wait_spoken(1, WAIT).await);
    for i in 2..=200 {
        r.focus(i, &format!("item {i}"));
    }
    assert!(voicego.wait_spoken(2, WAIT).await);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let spoken = voicego.spoken();
    assert_eq!(spoken.first().map(String::as_str), Some("first"));
    assert_eq!(spoken.last().map(String::as_str), Some("item 200"));
    assert!(
        spoken.len() <= 20,
        "a storm must not be replayed: {spoken:?}"
    );
}

#[tokio::test]
async fn cancel_and_speak_is_fast_and_in_order() {
    let d = tmp("c");
    let voicego = MockSpeechServer::start_voicego(&d).await.unwrap();
    voicego.set_speak_time(Duration::from_secs(5));
    let r = rig(vec![voicego_backend(voicego.socket_path().to_owned())]);
    r.focus(1, "one");
    assert!(voicego.wait_spoken(1, WAIT).await);
    // Let the Started event reach the lane.
    tokio::time::sleep(Duration::from_millis(30)).await;
    let mut worst = Duration::ZERO;
    for i in 2..12u64 {
        let t = r.focus(i, &format!("n{i}"));
        assert!(voicego.wait_spoken(usize::try_from(i).unwrap(), WAIT).await);
        let at = voicego.spoken_records().last().unwrap().at;
        worst = worst.max(at.saturating_duration_since(t));
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    eprintln!("focus -> server SPEAK (mock, loopback), worst of 10: {worst:?}");
    assert!(worst < Duration::from_millis(100), "{worst:?}");
    // The cancel and the new speak travelled together: STOP precedes each SPEAK.
    let verbs = voicego.verbs();
    assert!(verbs.iter().filter(|v| *v == "STOP self").count() >= 9);
}

#[tokio::test]
async fn voicego_down_falls_back_to_ssip_and_back() {
    let d = tmp("d");
    let mut voicego = MockSpeechServer::start_voicego(&d).await.unwrap();
    let d2 = tmp("d2");
    let ssip = MockSpeechServer::start_ssip(&d2).await.unwrap();
    voicego.stop().await;
    let mut r = rig(vec![
        voicego_backend(voicego.socket_path().to_owned()),
        ssip_backend(ssip.socket_path().to_owned()),
    ]);
    let n = tokio::time::timeout(WAIT, r.notices.recv()).await.unwrap();
    assert_eq!(
        n,
        Some(FailoverNotice::Fallback {
            from: "voicego",
            to: "ssip"
        })
    );
    r.focus(1, "via ssip");
    assert!(ssip.wait_spoken(1, WAIT).await);
    assert_eq!(ssip.spoken(), ["via ssip"]);

    voicego.restart().unwrap();
    let n = tokio::time::timeout(WAIT, r.notices.recv()).await.unwrap();
    assert_eq!(n, Some(FailoverNotice::Restored { backend: "voicego" }));
    r.focus(2, "back on voicego");
    assert!(voicego.wait_spoken(1, WAIT).await);
    assert_eq!(voicego.spoken(), ["back on voicego"]);
}

/// Regression (review F3): leaving the SSIP fallback must cancel what it was playing and quit,
/// or speech-dispatcher keeps talking over the returning voicego.
#[tokio::test]
async fn failback_cancels_and_quits_the_ssip_client_before_dropping_it() {
    let d = tmp("g");
    let mut voicego = MockSpeechServer::start_voicego(&d).await.unwrap();
    let d2 = tmp("g2");
    let ssip = MockSpeechServer::start_ssip(&d2).await.unwrap();
    ssip.set_speak_time(Duration::from_secs(30));
    voicego.stop().await;
    let mut r = rig(vec![
        voicego_backend(voicego.socket_path().to_owned()),
        ssip_backend(ssip.socket_path().to_owned()),
    ]);
    let n = tokio::time::timeout(WAIT, r.notices.recv()).await.unwrap();
    assert!(matches!(n, Some(FailoverNotice::Fallback { .. })));
    r.focus(1, "long speech on ssip");
    assert!(ssip.wait_spoken(1, WAIT).await);
    assert!(ssip.live_connections() >= 1);

    voicego.restart().unwrap();
    let n = tokio::time::timeout(WAIT, r.notices.recv()).await.unwrap();
    assert_eq!(n, Some(FailoverNotice::Restored { backend: "voicego" }));
    assert!(
        ssip.wait_verbs(WAIT, |v| v.iter().any(|l| l == "QUIT"))
            .await,
        "no QUIT: {:?}",
        ssip.verbs()
    );
    let v = ssip.verbs();
    let cancel = v
        .iter()
        .rposition(|l| l.starts_with("CANCEL"))
        .unwrap_or(usize::MAX);
    let quit = v.iter().position(|l| l == "QUIT").unwrap();
    assert!(cancel < quit, "CANCEL must come before QUIT: {v:?}");
    let end = Instant::now() + WAIT;
    while ssip.live_connections() != 0 {
        assert!(Instant::now() < end, "the SSIP connection was left open");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn voicego_dying_mid_session_switches_without_losing_the_next_utterance() {
    let d = tmp("e");
    let mut voicego = MockSpeechServer::start_voicego(&d).await.unwrap();
    let d2 = tmp("e2");
    let ssip = MockSpeechServer::start_ssip(&d2).await.unwrap();
    let mut r = rig(vec![
        voicego_backend(voicego.socket_path().to_owned()),
        ssip_backend(ssip.socket_path().to_owned()),
    ]);
    r.focus(1, "before");
    assert!(voicego.wait_spoken(1, WAIT).await);
    voicego.stop().await;
    let n = tokio::time::timeout(WAIT, r.notices.recv()).await.unwrap();
    assert!(matches!(n, Some(FailoverNotice::Fallback { .. })));
    r.focus(2, "after");
    assert!(ssip.wait_spoken(1, WAIT).await);
    assert_eq!(ssip.spoken(), ["after"]);
}

#[tokio::test]
async fn a_hung_speech_server_does_not_stall_the_producer() {
    let d = tmp("f");
    let voicego = MockSpeechServer::start_voicego(&d).await.unwrap();
    let r = rig(vec![voicego_backend(voicego.socket_path().to_owned())]);
    r.focus(1, "warm up");
    assert!(voicego.wait_spoken(1, WAIT).await);
    voicego.set_never_read(true);
    let big = "z".repeat(200_000);
    let t = Instant::now();
    for i in 2..300 {
        r.focus(i, &big);
    }
    assert!(
        t.elapsed() < Duration::from_millis(500),
        "submit blocked for {:?}",
        t.elapsed()
    );
}
