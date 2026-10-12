//! In-process fake `SpeechClient` and `SpeechBackend` for the lane and failover tests.

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gorca_output_api::{
    ClientHealth, SpeechCaps, SpeechClient, SpeechCmd, SpeechEvent, SubmitError,
};
use gorca_rt::{BoxFuture, Deadline, mpsc};

use crate::backend::{BackendConnectError, SpeechBackend};

fn h2u(h: ClientHealth) -> u8 {
    match h {
        ClientHealth::Connecting => 0,
        ClientHealth::Ready => 1,
        ClientHealth::Degraded => 2,
        ClientHealth::Down => 3,
    }
}

fn u2h(v: u8) -> ClientHealth {
    match v {
        0 => ClientHealth::Connecting,
        1 => ClientHealth::Ready,
        2 => ClientHealth::Degraded,
        _ => ClientHealth::Down,
    }
}

/// The test's handle on a [`FakeClient`].
#[derive(Clone, Debug)]
pub struct FakeCtl {
    pub cmds: Arc<Mutex<Vec<SpeechCmd>>>,
    pub health: Arc<AtomicU8>,
    pub events: mpsc::Sender<SpeechEvent>,
    /// `SpeechClient::close` was called.
    pub closed: Arc<AtomicBool>,
}

impl FakeCtl {
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    pub fn cmds(&self) -> Vec<SpeechCmd> {
        self.cmds.lock().unwrap().clone()
    }

    pub fn set_health(&self, h: ClientHealth) {
        self.health.store(h2u(h), Ordering::Release);
    }

    pub fn send(&self, ev: SpeechEvent) {
        self.events.try_send(ev).unwrap();
    }
}

#[derive(Debug)]
pub struct FakeClient {
    caps: SpeechCaps,
    ctl: FakeCtl,
    events: mpsc::Receiver<SpeechEvent>,
}

pub fn fake_client(atomic_replace: bool, health: ClientHealth) -> (FakeClient, FakeCtl) {
    let (tx, rx) = mpsc::channel(64);
    let ctl = FakeCtl {
        cmds: Arc::new(Mutex::new(Vec::new())),
        health: Arc::new(AtomicU8::new(h2u(health))),
        events: tx,
        closed: Arc::new(AtomicBool::new(false)),
    };
    (
        FakeClient {
            caps: SpeechCaps {
                atomic_replace,
                utterance_ids: true,
                max_in_flight: 1,
                index_marks: false,
                audio_progress: false,
                ssml: false,
            },
            ctl: ctl.clone(),
            events: rx,
        },
        ctl,
    )
}

impl SpeechClient for FakeClient {
    fn caps(&self) -> &SpeechCaps {
        &self.caps
    }

    fn submit(&self, cmd: SpeechCmd) -> Result<(), SubmitError> {
        if u2h(self.ctl.health.load(Ordering::Acquire)) == ClientHealth::Down {
            return Err(SubmitError::Disconnected);
        }
        self.ctl.cmds.lock().unwrap().push(cmd);
        Ok(())
    }

    fn events(&mut self) -> &mut mpsc::Receiver<SpeechEvent> {
        &mut self.events
    }

    fn health(&self) -> ClientHealth {
        u2h(self.ctl.health.load(Ordering::Acquire))
    }

    fn close(&self) {
        self.ctl.closed.store(true, Ordering::Release);
    }
}

/// A backend whose availability the test flips.
#[derive(Debug)]
pub struct FakeBackend {
    pub name: &'static str,
    pub available: Arc<AtomicBool>,
    pub connects: Arc<AtomicUsize>,
    pub clients: Arc<Mutex<Vec<FakeCtl>>>,
    pub atomic_replace: bool,
}

impl FakeBackend {
    pub fn new(name: &'static str, available: bool) -> Arc<Self> {
        Arc::new(Self {
            name,
            available: Arc::new(AtomicBool::new(available)),
            connects: Arc::new(AtomicUsize::new(0)),
            clients: Arc::new(Mutex::new(Vec::new())),
            atomic_replace: true,
        })
    }

    pub fn set_available(&self, on: bool) {
        self.available.store(on, Ordering::Release);
    }

    pub fn connects(&self) -> usize {
        self.connects.load(Ordering::Acquire)
    }

    /// The control handle of the newest client.
    pub fn last(&self) -> Option<FakeCtl> {
        self.clients.lock().unwrap().last().cloned()
    }
}

impl SpeechBackend for FakeBackend {
    fn name(&self) -> &'static str {
        self.name
    }

    fn connect(
        &self,
        _deadline: Deadline,
    ) -> BoxFuture<'static, Result<Box<dyn SpeechClient>, BackendConnectError>> {
        self.connects.fetch_add(1, Ordering::AcqRel);
        let ok = self.available.load(Ordering::Acquire);
        let name = self.name;
        let clients = Arc::clone(&self.clients);
        let atomic = self.atomic_replace;
        Box::pin(async move {
            if !ok {
                return Err(BackendConnectError::new(name, "refused"));
            }
            let (c, ctl) = fake_client(atomic, ClientHealth::Ready);
            clients.lock().unwrap().push(ctl);
            Ok(Box::new(c) as Box<dyn SpeechClient>)
        })
    }
}

/// Poll `cond` until it holds or `timeout` passes.
pub async fn eventually(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let end = tokio::time::Instant::now() + timeout;
    loop {
        if cond() {
            return true;
        }
        if tokio::time::Instant::now() >= end {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}
