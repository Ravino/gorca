// SPDX-License-Identifier: GPL-3.0-or-later
//! `FailoverClient`: an ordered list of speech backends behind one [`SpeechClient`] (TBD-V6).
//!
//! - On start the primary is tried under a Startup-class deadline. After
//!   [`FailoverConfig::max_failures`] failed attempts (50 ms doubling to 2 s between them) the
//!   next backend is used and a [`FailoverNotice::Fallback`] is emitted once.
//! - While running on a fallback the better backends are probed in the background (50 ms
//!   doubling to 2 s, then every [`FailoverConfig::probe_cap`]); when one answers, speech moves
//!   back to it ([`FailoverNotice::Restored`]).
//! - A backend that goes `Down` is reconnected with the same backoff before falling back.
//! - Whatever was in flight on a backend that is left behind is reported `Stopped`, so the
//!   speech state machine never waits for an `Ended` that cannot come. Commands queued longer
//!   than [`FailoverConfig::stale_after`] are dropped instead of replayed (section 7.4).
//! - Before a client is left behind (a better backend answered; at shutdown the client's own `Drop` does the same, best effort) it is told to
//!   `StopAll` and `close()` and given a bounded flush ([`RETIRE_FLUSH`]): speech-dispatcher keeps
//!   playing the queue of a client that merely disappears (review F3).
//! - `submit` never blocks: it pushes into a small bounded queue the supervisor drains. A stop
//!   always gets through the queue, speaks are evicted first and reported `Stopped` (F10).
//!
//! The facade advertises `atomic_replace = true`: a backend without atomic replace lowers
//! [`SpeechCmd::Replace`] to `STOP` + `SPEAK` itself (that is the contract of the command).

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::task::Poll;
use std::time::{Duration, Instant};

use gorca_model::UttId;
use gorca_output_api::queue::{admit, started_utt};
use gorca_output_api::{
    ClientHealth, SpeechCaps, SpeechClient, SpeechCmd, SpeechErr, SpeechEvent, SubmitError,
};
use gorca_rt::{BoxFuture, CancellationToken, Deadline, Notify, TaskSet, mpsc, with_deadline};

use crate::backend::{BackendConnectError, SpeechBackend};
use crate::state::backoff;

/// Commands waiting for the supervisor.
const QUEUE_CAP: usize = 8;
const EVENT_QUEUE: usize = 128;
const NOTICE_QUEUE: usize = 8;
const TERMINAL_SEND_DEADLINE: Duration = Duration::from_secs(1);
/// How long a client being left behind gets to flush `StopAll` (and `QUIT`) to its server.
pub const RETIRE_FLUSH: Duration = Duration::from_millis(50);

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FailoverMode {
    /// Backends in order; fall back and probe back.
    Auto,
    /// Exactly this backend (index into the list); no fallback.
    Only(usize),
}

#[derive(Clone, Debug)]
pub struct FailoverConfig {
    pub mode: FailoverMode,
    /// Deadline for one connect attempt (Startup class).
    pub connect_deadline: Duration,
    /// Failed attempts on one backend before the next one is tried.
    pub max_failures: u32,
    /// Queued commands older than this are dropped at a switch.
    pub stale_after: Duration,
    /// Probe interval for better backends once the backoff has reached its 2 s bound.
    pub probe_cap: Duration,
    /// How often a live client's health is looked at.
    pub health_poll: Duration,
}

impl Default for FailoverConfig {
    fn default() -> Self {
        Self {
            mode: FailoverMode::Auto,
            connect_deadline: Duration::from_secs(5),
            max_failures: 3,
            stale_after: Duration::from_secs(2),
            probe_cap: Duration::from_secs(5),
            health_poll: Duration::from_millis(25),
        }
    }
}

/// What the daemon announces to the user.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FailoverNotice {
    /// The primary is unavailable; speech now goes through `to`. Emitted once per episode.
    Fallback {
        from: &'static str,
        to: &'static str,
    },
    /// A better backend is back and in use again.
    Restored { backend: &'static str },
    /// No backend could be reached (emitted once until one comes back).
    Unavailable,
}

#[derive(Debug)]
struct Queued {
    cmd: SpeechCmd,
    at: Instant,
}

struct Shared {
    queue: Mutex<VecDeque<Queued>>,
    wake: Notify,
    health: AtomicU8,
    active: Mutex<Option<&'static str>>,
    /// Terminal events of commands evicted from `queue`, delivered by the supervisor.
    pending: Mutex<VecDeque<SpeechEvent>>,
}

impl std::fmt::Debug for Shared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shared").finish_non_exhaustive()
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

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

impl Shared {
    fn set_health(&self, h: ClientHealth) {
        self.health.store(h2u(h), Ordering::Release);
    }
}

#[derive(Debug)]
pub struct FailoverClient {
    caps: SpeechCaps,
    shared: Arc<Shared>,
    events: mpsc::Receiver<SpeechEvent>,
}

impl FailoverClient {
    /// Start the supervisor in `tasks` (its owner and cancellation path) and return the client
    /// plus the stream of notices to announce.
    pub fn spawn(
        tasks: &mut TaskSet,
        backends: Vec<Arc<dyn SpeechBackend>>,
        cfg: FailoverConfig,
    ) -> (FailoverClient, mpsc::Receiver<FailoverNotice>) {
        let shared = Arc::new(Shared {
            queue: Mutex::new(VecDeque::with_capacity(QUEUE_CAP)),
            wake: Notify::new(),
            health: AtomicU8::new(h2u(ClientHealth::Connecting)),
            active: Mutex::new(None),
            pending: Mutex::new(VecDeque::new()),
        });
        let (etx, erx) = mpsc::channel(EVENT_QUEUE);
        let (ntx, nrx) = mpsc::channel(NOTICE_QUEUE);
        let sup = Supervisor {
            shared: Arc::clone(&shared),
            backends,
            cfg,
            events: etx,
            notices: ntx,
            cancel: tasks.token(),
            inflight: Vec::new(),
            on_fallback: false,
            unavailable_announced: false,
        };
        tasks.spawn("speech-failover", sup.run());
        (
            FailoverClient {
                caps: SpeechCaps {
                    atomic_replace: true,
                    utterance_ids: true,
                    max_in_flight: 1,
                    index_marks: false,
                    audio_progress: false,
                    ssml: false,
                },
                shared,
                events: erx,
            },
            nrx,
        )
    }

    /// Name of the backend in use, if any.
    pub fn active_backend(&self) -> Option<&'static str> {
        *lock(&self.shared.active)
    }
}

impl SpeechClient for FailoverClient {
    fn caps(&self) -> &SpeechCaps {
        &self.caps
    }

    fn submit(&self, cmd: SpeechCmd) -> Result<(), SubmitError> {
        match u2h(self.shared.health.load(Ordering::Acquire)) {
            ClientHealth::Connecting | ClientHealth::Down => return Err(SubmitError::Disconnected),
            ClientHealth::Ready | ClientHealth::Degraded => {}
        }
        let gone = {
            let mut q = lock(&self.shared.queue);
            let item = Queued {
                cmd,
                at: Instant::now(),
            };
            admit(&mut q, item, QUEUE_CAP, |e| &e.cmd, |e| &mut e.cmd)?
        };
        if !gone.is_empty() {
            lock(&self.shared.pending).extend(gone.into_iter().map(SpeechEvent::Stopped));
        }
        self.shared.wake.notify_one();
        Ok(())
    }

    fn events(&mut self) -> &mut mpsc::Receiver<SpeechEvent> {
        &mut self.events
    }

    fn health(&self) -> ClientHealth {
        u2h(self.shared.health.load(Ordering::Acquire))
    }
}

type ConnectFut = BoxFuture<'static, Result<Box<dyn SpeechClient>, BackendConnectError>>;

/// Poll an optional pinned future; `Pending` forever when there is none.
fn poll_opt<T>(
    f: &mut Option<Pin<Box<dyn Future<Output = T> + Send>>>,
) -> impl Future<Output = T> + '_ {
    std::future::poll_fn(move |cx| match f.as_mut() {
        Some(fut) => fut.as_mut().poll(cx),
        None => Poll::Pending,
    })
}

enum Phase {
    /// Trying `idx`; `failures` attempts have failed so far.
    Connect { idx: usize, failures: u32 },
    /// Waiting before the next attempt.
    Retry {
        idx: usize,
        failures: u32,
        until: tokio::time::Instant,
    },
    Active {
        idx: usize,
        client: Box<dyn SpeechClient>,
    },
}

struct Supervisor {
    shared: Arc<Shared>,
    backends: Vec<Arc<dyn SpeechBackend>>,
    cfg: FailoverConfig,
    events: mpsc::Sender<SpeechEvent>,
    notices: mpsc::Sender<FailoverNotice>,
    cancel: CancellationToken,
    /// Utterances handed to the current client that have not ended.
    inflight: Vec<UttId>,
    on_fallback: bool,
    unavailable_announced: bool,
}

fn cmd_utt(cmd: &SpeechCmd) -> Option<UttId> {
    started_utt(cmd)
}

impl Supervisor {
    fn first_index(&self) -> usize {
        match self.cfg.mode {
            FailoverMode::Auto => 0,
            FailoverMode::Only(i) => i.min(self.backends.len().saturating_sub(1)),
        }
    }

    fn next_index(&self, idx: usize) -> Option<usize> {
        match self.cfg.mode {
            FailoverMode::Auto => (idx + 1 < self.backends.len()).then_some(idx + 1),
            FailoverMode::Only(_) => None,
        }
    }

    fn name(&self, idx: usize) -> &'static str {
        self.backends.get(idx).map_or("none", |b| b.name())
    }

    fn notice(&self, n: FailoverNotice) {
        if self.notices.try_send(n).is_err() {
            tracing::debug!(?n, "failover notice dropped (channel full or closed)");
        }
    }

    async fn emit(&self, ev: SpeechEvent) {
        let terminal = matches!(
            ev,
            SpeechEvent::Ended(_) | SpeechEvent::Stopped(_) | SpeechEvent::Error(..)
        );
        if terminal {
            let _ = tokio::time::timeout(TERMINAL_SEND_DEADLINE, self.events.send(ev)).await;
        } else {
            let _ = self.events.try_send(ev);
        }
    }

    /// Deliver the terminal events of commands that `submit` evicted.
    async fn deliver_pending(&self) {
        loop {
            let ev = lock(&self.shared.pending).pop_front();
            let Some(ev) = ev else { return };
            self.emit(ev).await;
        }
    }

    /// Leave a client behind: stop what it plays, say goodbye and give the bytes a moment to
    /// reach the server before the connection is dropped.
    async fn retire(&self, client: Box<dyn SpeechClient>) {
        let _ = client.submit(SpeechCmd::StopAll);
        client.close();
        let end = tokio::time::Instant::now() + RETIRE_FLUSH;
        while client.health() != ClientHealth::Down && tokio::time::Instant::now() < end {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }

    /// The client being left behind will never report these.
    async fn stop_inflight(&mut self) {
        for id in std::mem::take(&mut self.inflight) {
            self.emit(SpeechEvent::Stopped(id)).await;
        }
    }

    fn connect_fut(&self, idx: usize) -> Option<ConnectFut> {
        let b = self.backends.get(idx)?;
        let deadline = Deadline(Instant::now() + self.cfg.connect_deadline);
        let fut = b.connect(deadline);
        Some(Box::pin(async move {
            with_deadline(deadline, fut)
                .await
                .unwrap_or_else(|_| Err(BackendConnectError::new("connect", "deadline exceeded")))
        }))
    }

    async fn run(mut self) {
        if self.backends.is_empty() {
            self.shared.set_health(ClientHealth::Down);
            self.notice(FailoverNotice::Unavailable);
            self.cancel.cancelled().await;
            return;
        }
        let mut phase = Phase::Connect {
            idx: self.first_index(),
            failures: 0,
        };
        loop {
            phase = match phase {
                Phase::Connect { idx, failures } => self.connect_step(idx, failures).await,
                Phase::Retry {
                    idx,
                    failures,
                    until,
                } => {
                    tokio::select! {
                        biased;
                        () = self.cancel.cancelled() => return,
                        () = tokio::time::sleep_until(until) => {}
                    }
                    Phase::Connect { idx, failures }
                }
                Phase::Active { idx, client } => match self.active_step(idx, client).await {
                    Some(p) => p,
                    None => return,
                },
            };
            if self.cancel.is_cancelled() {
                return;
            }
        }
    }

    async fn connect_step(&mut self, idx: usize, failures: u32) -> Phase {
        let Some(fut) = self.connect_fut(idx) else {
            return Phase::Retry {
                idx: self.first_index(),
                failures: 0,
                until: tokio::time::Instant::now() + self.cfg.probe_cap,
            };
        };
        let res = tokio::select! {
            biased;
            () = self.cancel.cancelled() => return Phase::Retry {
                idx, failures, until: tokio::time::Instant::now(),
            },
            r = fut => r,
        };
        match res {
            Ok(client) => self.activated(idx, client),
            Err(e) => {
                tracing::debug!(backend = self.name(idx), error = %e, "speech backend unavailable");
                self.shared.set_health(ClientHealth::Down);
                *lock(&self.shared.active) = None;
                let failures = failures + 1;
                if failures >= self.cfg.max_failures {
                    if let Some(next) = self.next_index(idx) {
                        if !self.on_fallback {
                            self.on_fallback = true;
                            self.notice(FailoverNotice::Fallback {
                                from: self.name(self.first_index()),
                                to: self.name(next),
                            });
                        }
                        return Phase::Connect {
                            idx: next,
                            failures: 0,
                        };
                    }
                    // Everything failed: announce once, then start over from the best backend.
                    if !self.unavailable_announced {
                        self.unavailable_announced = true;
                        self.notice(FailoverNotice::Unavailable);
                    }
                    return Phase::Retry {
                        idx: self.first_index(),
                        failures: 0,
                        until: tokio::time::Instant::now() + self.cfg.probe_cap.min(backoff(6)),
                    };
                }
                Phase::Retry {
                    idx,
                    failures,
                    until: tokio::time::Instant::now() + backoff(failures),
                }
            }
        }
    }

    fn activated(&mut self, idx: usize, client: Box<dyn SpeechClient>) -> Phase {
        *lock(&self.shared.active) = Some(self.name(idx));
        self.shared.set_health(client.health());
        self.unavailable_announced = false;
        if idx == self.first_index() {
            if self.on_fallback {
                self.on_fallback = false;
                self.notice(FailoverNotice::Restored {
                    backend: self.name(idx),
                });
            }
        } else if !self.on_fallback {
            // Only reachable when the list is walked after an outage.
            self.on_fallback = true;
            self.notice(FailoverNotice::Fallback {
                from: self.name(self.first_index()),
                to: self.name(idx),
            });
        }
        Phase::Active { idx, client }
    }

    /// Forward queued commands and server events until the client dies or a better backend
    /// answers a probe. `None` means shutdown.
    async fn active_step(
        &mut self,
        idx: usize,
        mut client: Box<dyn SpeechClient>,
    ) -> Option<Phase> {
        let mut probe: Option<ConnectFut> = None;
        let mut probe_idx = 0usize;
        let mut probe_cursor = 0usize;
        let mut probe_attempt = 0u32;
        let mut next_probe = tokio::time::Instant::now() + backoff(1);
        let mut poll = tokio::time::interval(self.cfg.health_poll);
        poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            // Drain the queue first so a burst of wakeups is one pass.
            self.deliver_pending().await;
            if !self.flush(&mut *client).await {
                self.stop_inflight().await;
                self.shared.set_health(ClientHealth::Down);
                return Some(Phase::Connect { idx, failures: 1 });
            }
            let probing = idx > self.first_index() && probe.is_none();
            tokio::select! {
                biased;
                () = self.cancel.cancelled() => {
                    self.retire(client).await;
                    return None;
                }
                ev = client.events().recv() => {
                    if let Some(ev) = ev {
                        self.forward(ev).await;
                    } else {
                        self.stop_inflight().await;
                        self.shared.set_health(ClientHealth::Down);
                        return Some(Phase::Connect { idx, failures: 1 });
                    }
                }
                () = self.shared.wake.notified() => {}
                r = poll_opt(&mut probe), if probe.is_some() => {
                    probe = None;
                    match r {
                        Ok(better) => {
                            self.retire(client).await;
                            self.stop_inflight().await;
                            return Some(self.activated(probe_idx, better));
                        }
                        Err(e) => {
                            tracing::debug!(error = %e, "primary still unavailable");
                            probe_attempt += 1;
                            let wait = if probe_attempt < 7 {
                                backoff(probe_attempt + 1)
                            } else {
                                self.cfg.probe_cap
                            };
                            next_probe = tokio::time::Instant::now() + wait;
                        }
                    }
                }
                () = tokio::time::sleep_until(next_probe), if probing => {
                    // Several better backends are probed in turn.
                    probe_idx = probe_cursor % idx;
                    probe_cursor += 1;
                    probe = self.connect_fut(probe_idx);
                }
                _ = poll.tick() => {
                    let h = client.health();
                    self.shared.set_health(h);
                    if h == ClientHealth::Down {
                        self.stop_inflight().await;
                        return Some(Phase::Connect { idx, failures: 1 });
                    }
                }
            }
        }
    }

    async fn forward(&mut self, ev: SpeechEvent) {
        match &ev {
            SpeechEvent::Ended(id) | SpeechEvent::Stopped(id) | SpeechEvent::Error(id, _) => {
                self.inflight.retain(|u| u != id);
            }
            SpeechEvent::Accepted(_)
            | SpeechEvent::Started(_)
            | SpeechEvent::Mark(..)
            | SpeechEvent::AudioProgress(..) => {}
        }
        self.emit(ev).await;
    }

    /// Hand queued commands to the client. False when the client reported itself gone.
    async fn flush(&mut self, client: &mut dyn SpeechClient) -> bool {
        loop {
            let next = lock(&self.shared.queue).pop_front();
            let Some(q) = next else { return true };
            if q.at.elapsed() > self.cfg.stale_after {
                tracing::debug!("dropping a speech command older than the stale limit");
                if let Some(id) = cmd_utt(&q.cmd) {
                    self.emit(SpeechEvent::Stopped(id)).await;
                }
                continue;
            }
            let utt = cmd_utt(&q.cmd);
            match client.submit(q.cmd) {
                Ok(()) => {
                    if let Some(id) = utt {
                        self.inflight.push(id);
                        if self.inflight.len() > 16 {
                            self.inflight.remove(0);
                        }
                    }
                }
                Err(SubmitError::Full) => {
                    if let Some(id) = utt {
                        self.emit(SpeechEvent::Error(id, SpeechErr::Overload)).await;
                    }
                }
                Err(SubmitError::Disconnected) => {
                    if let Some(id) = utt {
                        self.emit(SpeechEvent::Stopped(id)).await;
                    }
                    return false;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{FakeBackend, eventually};
    use gorca_output_api::{Verbalized, VoiceRef};

    const WAIT: Duration = Duration::from_secs(3);

    fn cfg() -> FailoverConfig {
        FailoverConfig {
            connect_deadline: Duration::from_millis(500),
            probe_cap: Duration::from_millis(100),
            health_poll: Duration::from_millis(5),
            ..FailoverConfig::default()
        }
    }

    fn speak(id: u64) -> SpeechCmd {
        SpeechCmd::Speak {
            id: UttId(id),
            text: Verbalized {
                text: Arc::from("x"),
                lang: None,
                pitch_delta: 0,
            },
            voice: VoiceRef::Default,
            marks: false,
        }
    }

    fn dynb(b: &Arc<FakeBackend>) -> Arc<dyn SpeechBackend> {
        Arc::clone(b) as Arc<dyn SpeechBackend>
    }

    fn start(
        backends: Vec<Arc<dyn SpeechBackend>>,
        cfg: FailoverConfig,
    ) -> (FailoverClient, mpsc::Receiver<FailoverNotice>, TaskSet) {
        let mut tasks = TaskSet::new(CancellationToken::new());
        let (c, n) = FailoverClient::spawn(&mut tasks, backends, cfg);
        (c, n, tasks)
    }

    async fn ready(c: &FailoverClient) {
        assert!(
            eventually(WAIT, || matches!(
                c.health(),
                ClientHealth::Ready | ClientHealth::Degraded
            ))
            .await,
            "never became ready"
        );
    }

    #[tokio::test]
    async fn the_primary_is_used_when_it_is_up() {
        let a = FakeBackend::new("voicego", true);
        let b = FakeBackend::new("ssip", true);
        let (c, mut n, _t) = start(vec![dynb(&a), dynb(&b)], cfg());
        ready(&c).await;
        assert_eq!(c.active_backend(), Some("voicego"));
        c.submit(speak(1)).unwrap();
        let ctl = a.last().unwrap();
        assert!(eventually(WAIT, || ctl.cmds().len() == 1).await);
        assert_eq!(b.connects(), 0);
        assert!(n.try_recv().is_err());
    }

    #[tokio::test]
    async fn submit_before_a_backend_is_up_is_disconnected_and_never_blocks() {
        let a = FakeBackend::new("voicego", false);
        let (c, _n, _t) = start(vec![dynb(&a)], cfg());
        assert_eq!(c.submit(speak(1)), Err(SubmitError::Disconnected));
    }

    #[tokio::test]
    async fn an_unavailable_primary_falls_back_after_three_failures_and_announces_once() {
        let a = FakeBackend::new("voicego", false);
        let b = FakeBackend::new("ssip", true);
        let (c, mut n, _t) = start(vec![dynb(&a), dynb(&b)], cfg());
        ready(&c).await;
        assert_eq!(c.active_backend(), Some("ssip"));
        assert!(a.connects() >= 3);
        assert_eq!(
            n.recv().await,
            Some(FailoverNotice::Fallback {
                from: "voicego",
                to: "ssip"
            })
        );
        c.submit(speak(1)).unwrap();
        let ctl = b.last().unwrap();
        assert!(eventually(WAIT, || ctl.cmds().len() == 1).await);
        // Keep probing for a while: still only one notice.
        tokio::time::sleep(Duration::from_millis(250)).await;
        assert!(n.try_recv().is_err());
        assert!(a.connects() > 3, "the primary is probed in the background");
    }

    #[tokio::test]
    async fn the_primary_coming_back_takes_over_and_in_flight_speech_is_stopped() {
        let a = FakeBackend::new("voicego", false);
        let b = FakeBackend::new("ssip", true);
        let (mut c, mut n, _t) = start(vec![dynb(&a), dynb(&b)], cfg());
        ready(&c).await;
        assert!(matches!(
            n.recv().await,
            Some(FailoverNotice::Fallback { .. })
        ));
        c.submit(speak(7)).unwrap();
        let fb = b.last().unwrap();
        assert!(eventually(WAIT, || fb.cmds().len() == 1).await);
        a.set_available(true);
        assert_eq!(
            tokio::time::timeout(WAIT, n.recv()).await.unwrap(),
            Some(FailoverNotice::Restored { backend: "voicego" })
        );
        assert_eq!(c.active_backend(), Some("voicego"));
        // Utterance 7 was in flight on the fallback: the lane is told it is over.
        let ev = tokio::time::timeout(WAIT, c.events().recv()).await.unwrap();
        assert_eq!(ev, Some(SpeechEvent::Stopped(UttId(7))));
        c.submit(speak(8)).unwrap();
        let ctl = a.last().unwrap();
        assert!(eventually(WAIT, || ctl.cmds().len() == 1).await);
    }

    /// Regression (review F3): the fallback client is told to stop and closed before it is
    /// dropped, so speech-dispatcher does not keep playing under the returning primary.
    #[tokio::test]
    async fn the_client_left_behind_gets_stopall_and_close() {
        let a = FakeBackend::new("voicego", false);
        let b = FakeBackend::new("ssip", true);
        let (c, mut n, _t) = start(vec![dynb(&a), dynb(&b)], cfg());
        ready(&c).await;
        assert!(matches!(
            n.recv().await,
            Some(FailoverNotice::Fallback { .. })
        ));
        c.submit(speak(7)).unwrap();
        let fb = b.last().unwrap();
        assert!(eventually(WAIT, || fb.cmds().len() == 1).await);
        a.set_available(true);
        assert!(matches!(
            tokio::time::timeout(WAIT, n.recv()).await.unwrap(),
            Some(FailoverNotice::Restored { .. })
        ));
        assert_eq!(fb.cmds().last(), Some(&SpeechCmd::StopAll));
        assert!(fb.is_closed());
        assert!(a.last().is_some_and(|x| !x.is_closed()));
    }

    /// Regression (review F10): a stop is never lost in the facade's own queue either.
    #[tokio::test]
    async fn a_stop_is_never_lost_when_the_facade_queue_is_full() {
        let a = FakeBackend::new("voicego", true);
        let (mut c, _n, _t) = start(vec![dynb(&a)], cfg());
        ready(&c).await;
        // No wake-up between submits: hold the queue lock path by submitting in one burst
        // faster than the supervisor drains is racy, so exercise the admission directly.
        let mut q = VecDeque::new();
        for i in 0..QUEUE_CAP as u64 {
            admit(
                &mut q,
                Queued {
                    cmd: speak(i),
                    at: Instant::now(),
                },
                QUEUE_CAP,
                |e| &e.cmd,
                |e| &mut e.cmd,
            )
            .unwrap();
        }
        let gone = admit(
            &mut q,
            Queued {
                cmd: SpeechCmd::StopAll,
                at: Instant::now(),
            },
            QUEUE_CAP,
            |e| &e.cmd,
            |e| &mut e.cmd,
        )
        .unwrap();
        assert_eq!(gone.len(), QUEUE_CAP);
        assert_eq!(q.len(), 1);
        // And through the real client the stop reaches the backend.
        c.submit(SpeechCmd::StopAll).unwrap();
        let ctl = a.last().unwrap();
        assert!(eventually(WAIT, || ctl.cmds().contains(&SpeechCmd::StopAll)).await);
        let _ = c.events();
    }

    #[tokio::test]
    async fn a_client_that_goes_down_is_reconnected_on_the_same_backend() {
        let a = FakeBackend::new("voicego", true);
        let b = FakeBackend::new("ssip", true);
        let (c, mut n, _t) = start(vec![dynb(&a), dynb(&b)], cfg());
        ready(&c).await;
        a.last().unwrap().set_health(ClientHealth::Down);
        assert!(eventually(WAIT, || a.connects() == 2).await);
        ready(&c).await;
        assert_eq!(c.active_backend(), Some("voicego"));
        assert_eq!(b.connects(), 0);
        assert!(n.try_recv().is_err(), "a blip is not announced");
    }

    #[tokio::test]
    async fn forwarded_events_reach_the_consumer() {
        let a = FakeBackend::new("voicego", true);
        let (mut c, _n, _t) = start(vec![dynb(&a)], cfg());
        ready(&c).await;
        a.last().unwrap().send(SpeechEvent::Ended(UttId(3)));
        let ev = tokio::time::timeout(WAIT, c.events().recv()).await.unwrap();
        assert_eq!(ev, Some(SpeechEvent::Ended(UttId(3))));
    }

    #[tokio::test]
    async fn only_mode_never_falls_back() {
        let a = FakeBackend::new("voicego", false);
        let b = FakeBackend::new("ssip", true);
        let mut cfg = cfg();
        cfg.mode = FailoverMode::Only(0);
        let (c, mut n, _t) = start(vec![dynb(&a), dynb(&b)], cfg);
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(c.health(), ClientHealth::Down);
        assert_eq!(b.connects(), 0);
        assert!(a.connects() >= 3);
        // Down is announced once.
        assert_eq!(n.try_recv(), Ok(FailoverNotice::Unavailable));
        assert!(n.try_recv().is_err());
    }

    #[tokio::test]
    async fn no_backends_at_all_is_down() {
        let (c, mut n, _t) = start(Vec::new(), cfg());
        assert_eq!(n.recv().await, Some(FailoverNotice::Unavailable));
        assert_eq!(c.health(), ClientHealth::Down);
    }

    #[tokio::test]
    async fn the_supervisor_ends_when_its_owner_cancels() {
        let a = FakeBackend::new("voicego", true);
        let (c, _n, mut t) = start(vec![dynb(&a)], cfg());
        ready(&c).await;
        let reports = t.shutdown(Duration::from_secs(1)).await;
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].exit, gorca_rt::TaskExit::Completed);
    }
}
