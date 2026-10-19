// SPDX-License-Identifier: GPL-3.0-or-later
//! Pointer emulation through the real core runtime with a mock `PointerSink` (ADR-0025 MO-4):
//! availability reaches the Reader, a key becomes a sink call with a deadline, the answer comes
//! back as a message, and a stalled sink never delays the spoken acknowledgement (EX-1, EX-4).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::disallowed_methods
)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use gorca_core::{ControlMsg, Core, CoreCmd, CoreConfig, SpeechOut};
use gorca_input_api::{KeyClass, KeyEvent, KeyIntent, Modifiers, PressKind};
use gorca_model::{Epoch, Utterance};
use gorca_output_api::{
    Button, ButtonAct, ClickOpts, OutcomeKind, PointTarget, PointerAvail, PointerError,
    PointerOutcome, PointerPos, PointerSink,
};
use gorca_rt::{BoxFuture, CancellationToken, Deadline, watch};

#[derive(Debug, Default)]
struct Recording {
    spoken: Mutex<Vec<(Instant, String)>>,
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

/// A sink that records calls and can be told to stall.
struct Mock {
    avail: watch::Sender<PointerAvail>,
    calls: Mutex<Vec<String>>,
    held: Mutex<u32>,
    stall: AtomicBool,
}

impl Mock {
    fn new(a: PointerAvail) -> Arc<Self> {
        let (avail, _) = watch::channel(a);
        Arc::new(Self {
            avail,
            calls: Mutex::new(Vec::new()),
            held: Mutex::new(0),
            stall: AtomicBool::new(false),
        })
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    fn outcome(&self) -> PointerOutcome {
        PointerOutcome {
            kind: OutcomeKind::Done,
            x: 1.0,
            y: 2.0,
            window_at: 0,
            occluded: false,
            clamped: false,
            held: *self.held.lock().unwrap(),
        }
    }
}

impl PointerSink for Mock {
    fn click_at(
        &self,
        _t: PointTarget,
        _b: Button,
        _o: ClickOpts,
        _d: Deadline,
    ) -> BoxFuture<'_, Result<PointerOutcome, PointerError>> {
        Box::pin(async { Err(PointerError::NoLocation) })
    }

    fn move_to(
        &self,
        _t: PointTarget,
        _o: ClickOpts,
        _d: Deadline,
    ) -> BoxFuture<'_, Result<PointerOutcome, PointerError>> {
        Box::pin(async { Err(PointerError::NoLocation) })
    }

    fn button(
        &self,
        b: Button,
        act: ButtonAct,
        d: Deadline,
    ) -> BoxFuture<'_, Result<PointerOutcome, PointerError>> {
        Box::pin(async move {
            let ahead = d.remaining(Instant::now());
            self.calls.lock().unwrap().push(format!(
                "button {b:?} {act:?} deadline<=150ms:{}",
                ahead <= Duration::from_millis(150)
            ));
            if self.stall.load(Ordering::Relaxed) {
                // The helper's own deadline handling: the client answers Timeout at the deadline.
                tokio::time::sleep(ahead).await;
                return Err(PointerError::Timeout);
            }
            let bit = b.held_bit();
            match act {
                ButtonAct::Press => *self.held.lock().unwrap() |= bit,
                ButtonAct::Release => *self.held.lock().unwrap() &= !bit,
                ButtonAct::Click | ButtonAct::DoubleClick => {}
            }
            Ok(self.outcome())
        })
    }

    fn release_all(&self, _d: Deadline) -> BoxFuture<'_, Result<u32, PointerError>> {
        Box::pin(async move {
            self.calls.lock().unwrap().push("release_all".to_owned());
            let n = self.held.lock().unwrap().count_ones();
            *self.held.lock().unwrap() = 0;
            Ok(n)
        })
    }

    fn position(&self, _d: Deadline) -> BoxFuture<'_, Result<PointerPos, PointerError>> {
        Box::pin(async {
            Ok(PointerPos {
                x: 5.0,
                y: 6.0,
                window: 0,
                wx: 0.0,
                wy: 0.0,
                monitor: 0,
            })
        })
    }

    fn availability(&self) -> watch::Receiver<PointerAvail> {
        self.avail.subscribe()
    }
}

struct Rig {
    speech: Arc<Recording>,
    mock: Arc<Mock>,
    senders: gorca_core::ReaderSenders,
    shutdown: CancellationToken,
    core: tokio::task::JoinHandle<Result<(), gorca_core::CoreError>>,
    epoch: u64,
}

fn rig(a: PointerAvail) -> Rig {
    let speech = Arc::new(Recording::default());
    let mock = Mock::new(a);
    let core = Core::new(CoreConfig::default())
        .with_speech(Arc::clone(&speech) as Arc<dyn SpeechOut>)
        .with_pointer(Arc::clone(&mock) as Arc<dyn PointerSink>);
    let senders = core.reader_senders();
    let shutdown = CancellationToken::new();
    let core = tokio::spawn(core.run(shutdown.clone()));
    Rig {
        speech,
        mock,
        senders,
        shutdown,
        core,
        epoch: 0,
    }
}

impl Rig {
    async fn press(&mut self, cmd: CoreCmd, repeat: u8) {
        self.epoch += 1;
        let intent = KeyIntent {
            event: KeyEvent {
                keycode: 98,
                keysym: 0xffaf,
                unichar: 0,
                modifiers: Modifiers::empty(),
                pressed: true,
            },
            kind: PressKind::Press,
            class: KeyClass::Command(cmd.id()),
            repeat,
            chord: None,
            epoch: Epoch(self.epoch),
            at: Instant::now(),
        };
        self.senders.key.send(intent).await.unwrap();
    }

    async fn wait_text(&self, text: &str, secs: u64) -> Option<Instant> {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            let found = self
                .speech
                .spoken
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .iter()
                .find(|(_, t)| t == text)
                .map(|(at, _)| *at);
            if found.is_some() || Instant::now() > deadline {
                return found;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }

    async fn stop(self) {
        self.shutdown.cancel();
        let _ = self.core.await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn availability_reaches_the_reader_and_a_click_reaches_the_sink() {
    let mut r = rig(PointerAvail::Absent);
    // The helper is absent: the command says why and the sink sees nothing.
    r.press(CoreCmd::MouseLeftClick, 1).await;
    assert!(
        r.wait_text("Mouse not available: gorca helper is not running", 3)
            .await
            .is_some()
    );
    assert!(r.mock.calls().is_empty());
    // It appears: availability is forwarded to the Reader.
    r.mock
        .avail
        .send(PointerAvail::Ready { version: 2 })
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    r.press(CoreCmd::MouseLeftClick, 1).await;
    assert!(r.wait_text("Left click", 3).await.is_some());
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(r.mock.calls(), ["button Left Click deadline<=150ms:true"]);
    r.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stalled_sink_never_delays_the_acknowledgement_and_the_failure_is_spoken() {
    let mut r = rig(PointerAvail::Ready { version: 2 });
    tokio::time::sleep(Duration::from_millis(50)).await;
    r.mock.stall.store(true, Ordering::Relaxed);
    let t0 = Instant::now();
    r.press(CoreCmd::MouseRightClick, 1).await;
    let said = r.wait_text("Right click", 3).await.expect("acknowledged");
    assert!(
        said - t0 < Duration::from_millis(60),
        "acknowledged after {:?}",
        said - t0
    );
    let failed = r
        .wait_text("Mouse not responding", 3)
        .await
        .expect("failure spoken");
    assert!(
        failed - t0 >= Duration::from_millis(100),
        "the sink's deadline is ~150 ms"
    );
    r.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_lock_round_trips_through_the_sink() {
    let mut r = rig(PointerAvail::Ready { version: 2 });
    tokio::time::sleep(Duration::from_millis(50)).await;
    r.press(CoreCmd::MouseLeftLockToggle, 1).await;
    assert!(r.wait_text("Left button locked", 3).await.is_some());
    r.press(CoreCmd::MouseLeftLockToggle, 1).await;
    assert!(r.wait_text("Left button unlocked", 3).await.is_some());
    assert_eq!(
        r.mock.calls(),
        [
            "button Left Press deadline<=150ms:true",
            "button Left Release deadline<=150ms:true"
        ]
    );
    r.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn say_position_uses_the_sink() {
    let mut r = rig(PointerAvail::Ready { version: 2 });
    tokio::time::sleep(Duration::from_millis(50)).await;
    r.press(CoreCmd::MouseSayPosition, 1).await;
    assert!(r.wait_text("Screen 5, 6, monitor 1", 3).await.is_some());
    let _ = ControlMsg::Pointer(gorca_core::PointerMsg::Enabled(true)); // the public message type exists
    r.stop().await;
}
