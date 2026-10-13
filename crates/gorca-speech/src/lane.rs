//! The speech lane (ADR-0001 sections 7.4, 8.1, 8.3): turns utterances into `SpeechCmd`s.
//!
//! Producers (the Reader, `PresentJob`s) call [`SpeechHandle::submit`], which never blocks:
//!
//! - an interrupting utterance (priority `Interrupt`, or an epoch newer than what is being
//!   spoken) goes into a **single slot**; a newer one replaces an older one that the lane has
//!   not picked up yet, so a focus storm leaves only the last focus to be spoken;
//! - everything else goes into a bounded FIFO (32), dropping the newest when full.
//!
//! After advancing the `EpochCell` (a key press, a mode change) the producer calls
//! [`SpeechHandle::notify_epoch`]. The lane then runs the pure [`SpeechQueue`] state machine,
//! re-checks E-2 (`Utterance::is_valid`) when it is about to send, and turns effects into
//! commands for the client. Cancel + speak is one `SpeechCmd::Replace` (one write on a
//! backend with atomic replace).

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use gorca_model::{Epoch, Prio, UttId, Utterance};
use gorca_output_api::{
    ClientHealth, SpeechClient, SpeechCmd, SpeechEvent, SubmitError, VoiceParams, VoiceRef,
};
use gorca_rt::{EpochCell, Notify, TaskSet};

use crate::queue::{FIFO_CAPACITY, SpeechQueue};
use crate::state::{SpeechEffect, SpeechMsg, SpeechState, UttMeta};
use crate::verbalize::Verbalizer;

/// Speech control, applied by the lane before the utterances that follow it (ADR-0005 6.2).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SpeechControl {
    /// Voice parameters; also re-sent after a reconnect.
    SetParams(VoiceParams),
    Pause,
    Resume,
    /// While muted, new utterances are dropped at `submit`. Utterances already submitted are
    /// still spoken, so "speech off" can be heard.
    Mute(bool),
}

/// Controls queued at once; the oldest is dropped beyond this (they are idempotent toggles).
const CONTROL_CAPACITY: usize = 16;

#[derive(Clone, Debug)]
pub struct LaneConfig {
    /// Health poll interval while connected.
    pub idle_poll: Duration,
    /// Health poll interval while disconnected.
    pub down_poll: Duration,
    /// Re-sent after a reconnect (`ResendVoiceParams`).
    pub voice_params: Option<VoiceParams>,
}

impl Default for LaneConfig {
    fn default() -> Self {
        Self {
            idle_poll: Duration::from_millis(250),
            down_poll: Duration::from_millis(25),
            voice_params: None,
        }
    }
}

/// Counters, for tests and telemetry.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct SpeechStats {
    /// Utterances handed to the client (as `Speak` or as the `then` of a `Replace`).
    pub spoken: u64,
    /// Of those, how many replaced a running utterance.
    pub replaced: u64,
    /// Utterances dropped because their epoch was stale (E-2), at submit or at send time.
    pub stale_dropped: u64,
    /// Replaces that showed neither `Started` nor `Ended` within the 30 ms deadline.
    pub replace_slow: u64,
    /// Interrupts overwritten in the slot before the lane saw them.
    pub superseded: u64,
    /// Utterances dropped because the inbox FIFO was full.
    pub inbox_dropped: u64,
    /// Utterances dropped at submit because speech is muted.
    pub muted_dropped: u64,
    /// Commands the client refused (`Full`).
    pub submit_failed: u64,
}

#[derive(Debug, Default)]
struct Counters {
    spoken: AtomicU64,
    replaced: AtomicU64,
    stale_dropped: AtomicU64,
    replace_slow: AtomicU64,
    superseded: AtomicU64,
    inbox_dropped: AtomicU64,
    muted_dropped: AtomicU64,
    submit_failed: AtomicU64,
}

#[derive(Debug)]
struct LaneShared {
    slot: Mutex<Option<Utterance>>,
    inbox: Mutex<VecDeque<Utterance>>,
    controls: Mutex<VecDeque<SpeechControl>>,
    muted: AtomicBool,
    wake: Notify,
    epoch: Arc<EpochCell>,
    /// Epoch of the utterance last handed to the client.
    inflight_epoch: AtomicU64,
    stats: Counters,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Producer side of the lane. Cheap to clone; every method is non-blocking.
#[derive(Clone, Debug)]
pub struct SpeechHandle {
    shared: Arc<LaneShared>,
}

impl SpeechHandle {
    /// Queue an utterance. Never blocks, never fails: a stale or surplus utterance is dropped
    /// and counted.
    pub fn submit(&self, u: Utterance) {
        let s = &self.shared;
        if s.muted.load(Ordering::Acquire) {
            s.stats.muted_dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        if !u.is_valid(s.epoch.current()) {
            s.stats.stale_dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let interrupts =
            u.prio == Prio::Interrupt || u.epoch.0 > s.inflight_epoch.load(Ordering::Acquire);
        if interrupts {
            let mut slot = lock(&s.slot);
            match slot.as_ref() {
                Some(old) if old.epoch > u.epoch => {
                    // An older interrupt arrived late: the one in the slot is newer.
                    s.stats.stale_dropped.fetch_add(1, Ordering::Relaxed);
                    return;
                }
                Some(_) => {
                    s.stats.superseded.fetch_add(1, Ordering::Relaxed);
                }
                None => {}
            }
            *slot = Some(u);
        } else {
            let mut q = lock(&s.inbox);
            if q.len() >= FIFO_CAPACITY {
                s.stats.inbox_dropped.fetch_add(1, Ordering::Relaxed);
                return;
            }
            q.push_back(u);
        }
        s.wake.notify_one();
    }

    /// Queue a control. Never blocks. `Mute` takes effect for later `submit`s at once.
    pub fn control(&self, c: SpeechControl) {
        let s = &self.shared;
        if let SpeechControl::Mute(m) = c {
            s.muted.store(m, Ordering::Release);
        }
        {
            let mut q = lock(&s.controls);
            if q.len() >= CONTROL_CAPACITY {
                q.pop_front();
            }
            q.push_back(c);
        }
        s.wake.notify_one();
    }

    /// The `EpochCell` was advanced: wake the lane so it cancels stale speech now.
    pub fn notify_epoch(&self) {
        self.shared.wake.notify_one();
    }

    pub fn stats(&self) -> SpeechStats {
        let c = &self.shared.stats;
        let g = |a: &AtomicU64| a.load(Ordering::Relaxed);
        SpeechStats {
            spoken: g(&c.spoken),
            replaced: g(&c.replaced),
            stale_dropped: g(&c.stale_dropped),
            replace_slow: g(&c.replace_slow),
            superseded: g(&c.superseded),
            inbox_dropped: g(&c.inbox_dropped),
            muted_dropped: g(&c.muted_dropped),
            submit_failed: g(&c.submit_failed),
        }
    }
}

#[derive(Debug)]
pub struct SpeechLane;

impl SpeechLane {
    /// Run the lane in `tasks` (its owner and cancellation path).
    pub fn spawn(
        tasks: &mut TaskSet,
        epoch: Arc<EpochCell>,
        client: Box<dyn SpeechClient>,
        cfg: LaneConfig,
    ) -> SpeechHandle {
        let shared = Arc::new(LaneShared {
            slot: Mutex::new(None),
            inbox: Mutex::new(VecDeque::with_capacity(FIFO_CAPACITY)),
            controls: Mutex::new(VecDeque::new()),
            muted: AtomicBool::new(false),
            wake: Notify::new(),
            epoch,
            inflight_epoch: AtomicU64::new(0),
            stats: Counters::default(),
        });
        let lane = Lane::new(Arc::clone(&shared), client, cfg);
        tasks.spawn("speech-lane", lane.run());
        SpeechHandle { shared }
    }
}

fn connected(h: ClientHealth) -> bool {
    match h {
        ClientHealth::Ready | ClientHealth::Degraded => true,
        ClientHealth::Connecting | ClientHealth::Down => false,
    }
}

/// Bound on state-machine messages produced by one wake-up (feedback loops cannot spin).
const MAX_FOLLOWUPS: usize = 64;

struct Lane {
    shared: Arc<LaneShared>,
    client: Box<dyn SpeechClient>,
    cfg: LaneConfig,
    queue: SpeechQueue,
    store: HashMap<UttId, Utterance>,
    verbalizer: Verbalizer,
    seen_epoch: Epoch,
    was_connected: bool,
    followups: VecDeque<SpeechMsg>,
}

impl Lane {
    fn new(shared: Arc<LaneShared>, client: Box<dyn SpeechClient>, cfg: LaneConfig) -> Self {
        let atomic = client.caps().atomic_replace;
        let seen_epoch = shared.epoch.current();
        Self {
            shared,
            client,
            cfg,
            queue: SpeechQueue::new(atomic),
            store: HashMap::new(),
            verbalizer: Verbalizer,
            seen_epoch,
            was_connected: true,
            followups: VecDeque::new(),
        }
    }

    async fn run(mut self) {
        // Start disconnected if the client is not up yet: intents queue in the state machine.
        self.was_connected = connected(self.client.health());
        if !self.was_connected {
            self.feed(SpeechMsg::TransportLost);
        }
        let mut events_closed = false;
        loop {
            let poll = if self.was_connected {
                self.cfg.idle_poll
            } else {
                self.cfg.down_poll
            };
            let tick_at = match self.queue.state() {
                SpeechState::Replacing { deadline, .. } => Some(deadline),
                SpeechState::Idle
                | SpeechState::Speaking { .. }
                | SpeechState::Paused { .. }
                | SpeechState::Disconnected { .. } => None,
            };
            let sleep = match tick_at {
                Some(d) => d.saturating_duration_since(Instant::now()).min(poll),
                None => poll,
            };
            let shared = Arc::clone(&self.shared);
            tokio::select! {
                biased;
                ev = self.client.events().recv(), if !events_closed => {
                    if let Some(ev) = ev {
                        self.on_event(&ev);
                    } else {
                        // The client is gone for good: no busy loop on a closed channel.
                        events_closed = true;
                        self.feed(SpeechMsg::TransportLost);
                    }
                }
                () = shared.wake.notified() => {}
                () = tokio::time::sleep(sleep) => {}
            }
            self.pump();
        }
    }

    fn on_event(&mut self, ev: &SpeechEvent) {
        match *ev {
            SpeechEvent::Started(id) => self.feed(SpeechMsg::Started(id)),
            SpeechEvent::Ended(id) => {
                self.store.remove(&id);
                self.feed(SpeechMsg::Ended(id));
            }
            SpeechEvent::Stopped(id) => {
                self.store.remove(&id);
                self.feed(SpeechMsg::Stopped(id));
            }
            SpeechEvent::Error(id, _) => {
                self.store.remove(&id);
                self.feed(SpeechMsg::Failed(id));
            }
            SpeechEvent::Accepted(_) | SpeechEvent::Mark(..) | SpeechEvent::AudioProgress(..) => {}
        }
    }

    /// Everything that may have changed since the last wake-up.
    fn pump(&mut self) {
        // Controls first: "rate 55" is spoken at rate 55 (ADR-0005 6.2).
        let controls: Vec<SpeechControl> = lock(&self.shared.controls).drain(..).collect();
        for c in controls {
            self.apply_control(c);
        }
        // New intents first: an interrupt that arrives together with the epoch advance that
        // made the running utterance stale becomes one `Replace`, not `Stop` then `Speak`.
        let slot = lock(&self.shared.slot).take();
        let fifo: Vec<Utterance> = lock(&self.shared.inbox).drain(..).collect();
        for u in slot.into_iter().chain(fifo) {
            self.store.insert(u.id, u.clone());
            self.feed(SpeechMsg::Intent(UttMeta::from(&u)));
        }
        let now_epoch = self.shared.epoch.current();
        if now_epoch != self.seen_epoch {
            self.seen_epoch = now_epoch;
            self.feed(SpeechMsg::EpochAdvanced);
        }
        let now_connected = connected(self.client.health());
        if now_connected != self.was_connected {
            self.was_connected = now_connected;
            self.feed(if now_connected {
                SpeechMsg::Reconnected
            } else {
                SpeechMsg::TransportLost
            });
        }
        if matches!(self.queue.state(), SpeechState::Replacing { .. }) {
            self.feed(SpeechMsg::Tick);
        }
        // Utterances that were never handed on and are not tracked any more must not pile up.
        if self.store.len() > 4 * FIFO_CAPACITY {
            self.store.clear();
        }
    }

    fn apply_control(&mut self, c: SpeechControl) {
        match c {
            SpeechControl::SetParams(p) => {
                self.cfg.voice_params = Some(p);
                self.send(SpeechCmd::SetParams(p), None);
            }
            SpeechControl::Pause => self.feed(SpeechMsg::Pause),
            SpeechControl::Resume => self.feed(SpeechMsg::Resume),
            // Applied at `submit`; nothing to do on the lane.
            SpeechControl::Mute(_) => {}
        }
    }

    /// Feed one message and everything it causes.
    fn feed(&mut self, msg: SpeechMsg) {
        self.followups.push_back(msg);
        let mut budget = MAX_FOLLOWUPS;
        while let Some(m) = self.followups.pop_front() {
            if budget == 0 {
                self.followups.clear();
                break;
            }
            budget -= 1;
            let effects = self
                .queue
                .handle(m, Instant::now(), self.shared.epoch.current());
            for e in effects {
                self.exec(e);
            }
        }
    }

    fn voice_cmd(&self, u: &Utterance) -> SpeechCmd {
        SpeechCmd::Speak {
            id: u.id,
            text: self.verbalizer.verbalize(u),
            voice: VoiceRef::Default,
            marks: false,
        }
    }

    /// The utterance for `id` if it may still be spoken (E-2 at send time).
    fn sendable(&mut self, id: UttId) -> Option<Utterance> {
        let u = self.store.get(&id)?.clone();
        if u.is_valid(self.shared.epoch.current()) {
            Some(u)
        } else {
            self.shared
                .stats
                .stale_dropped
                .fetch_add(1, Ordering::Relaxed);
            self.store.remove(&id);
            // The state machine thinks it started this one: tell it that it ended.
            self.followups.push_back(SpeechMsg::Stopped(id));
            None
        }
    }

    fn send(&mut self, cmd: SpeechCmd, utt: Option<&Utterance>) {
        match self.client.submit(cmd) {
            Ok(()) => {
                if let Some(u) = utt {
                    self.shared.stats.spoken.fetch_add(1, Ordering::Relaxed);
                    self.shared
                        .inflight_epoch
                        .store(u.epoch.0, Ordering::Release);
                }
            }
            Err(SubmitError::Full) => {
                self.shared
                    .stats
                    .submit_failed
                    .fetch_add(1, Ordering::Relaxed);
                if let Some(u) = utt {
                    self.store.remove(&u.id);
                    self.followups.push_back(SpeechMsg::Failed(u.id));
                }
            }
            Err(SubmitError::Disconnected) => {
                self.shared
                    .stats
                    .submit_failed
                    .fetch_add(1, Ordering::Relaxed);
                if let Some(u) = utt {
                    self.store.remove(&u.id);
                }
                self.followups.push_back(SpeechMsg::TransportLost);
            }
        }
    }

    fn exec(&mut self, e: SpeechEffect) {
        match e {
            SpeechEffect::Speak(id) => {
                if let Some(u) = self.sendable(id) {
                    let cmd = self.voice_cmd(&u);
                    self.send(cmd, Some(&u));
                }
            }
            SpeechEffect::Replace { stop, then } => {
                self.store.remove(&stop);
                match self.sendable(then) {
                    Some(u) => {
                        let cmd = SpeechCmd::Replace {
                            stop,
                            then: Box::new(self.voice_cmd(&u)),
                        };
                        self.shared.stats.replaced.fetch_add(1, Ordering::Relaxed);
                        self.send(cmd, Some(&u));
                    }
                    // The newcomer is already stale: just silence the old one.
                    None => self.send(SpeechCmd::Stop { id: stop }, None),
                }
            }
            SpeechEffect::Stop(id) => {
                self.store.remove(&id);
                self.send(SpeechCmd::Stop { id }, None);
            }
            SpeechEffect::StopAll => self.send(SpeechCmd::StopAll, None),
            SpeechEffect::Pause => self.send(SpeechCmd::Pause, None),
            SpeechEffect::Resume => self.send(SpeechCmd::Resume, None),
            SpeechEffect::Discard(id) => {
                self.store.remove(&id);
            }
            SpeechEffect::CountReplaceSlow => {
                self.shared
                    .stats
                    .replace_slow
                    .fetch_add(1, Ordering::Relaxed);
            }
            SpeechEffect::ResendVoiceParams => {
                if let Some(p) = self.cfg.voice_params {
                    self.send(SpeechCmd::SetParams(p), None);
                }
            }
            // Reconnection is the client's job (FailoverClient); the queue consumed the rest.
            SpeechEffect::Reconnect
            | SpeechEffect::Enqueue(_)
            | SpeechEffect::PurgeStale
            | SpeechEffect::PopNext
            | SpeechEffect::DiscardOlderThan(_) => {}
        }
    }
}

#[cfg(test)]
#[allow(clippy::wildcard_enum_match_arm)]
mod tests {
    use super::*;
    use crate::fake::{FakeCtl, eventually, fake_client};
    use gorca_model::{Segment, SegmentKind};
    use smallvec::smallvec;

    const WAIT: Duration = Duration::from_secs(2);

    fn utt(id: u64, epoch: u64, prio: Prio, text: &str) -> Utterance {
        Utterance {
            id: UttId(id),
            segments: smallvec![Segment {
                text: Arc::from(text),
                kind: SegmentKind::Text
            }],
            lang: None,
            epoch: Epoch(epoch),
            prio,
            may_refine: false,
            survives_focus_change: false,
            refines: None,
        }
    }

    fn start(
        atomic: bool,
        health: ClientHealth,
    ) -> (SpeechHandle, FakeCtl, Arc<EpochCell>, TaskSet) {
        let (client, ctl) = fake_client(atomic, health);
        let epoch = Arc::new(EpochCell::new());
        let mut tasks = TaskSet::new(gorca_rt::CancellationToken::new());
        let cfg = LaneConfig {
            down_poll: Duration::from_millis(5),
            idle_poll: Duration::from_millis(5),
            ..LaneConfig::default()
        };
        let h = SpeechLane::spawn(&mut tasks, Arc::clone(&epoch), Box::new(client), cfg);
        (h, ctl, epoch, tasks)
    }

    fn speaks(ctl: &FakeCtl) -> Vec<u64> {
        ctl.cmds()
            .iter()
            .filter_map(|c| match c {
                SpeechCmd::Speak { id, .. } => Some(id.0),
                SpeechCmd::Replace { then, .. } => match &**then {
                    SpeechCmd::Speak { id, .. } => Some(id.0),
                    _ => None,
                },
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn first_utterance_is_a_plain_speak_with_verbalized_text() {
        let (h, ctl, epoch, _t) = start(true, ClientHealth::Ready);
        let e = epoch.advance();
        h.submit(utt(1, e.0, Prio::Interrupt, "OK"));
        assert!(eventually(WAIT, || !ctl.cmds().is_empty()).await);
        match &ctl.cmds()[0] {
            SpeechCmd::Speak { id, text, .. } => {
                assert_eq!(*id, UttId(1));
                assert_eq!(&*text.text, "OK");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_focus_storm_speaks_only_the_last_focus() {
        let (h, ctl, epoch, _t) = start(true, ClientHealth::Ready);
        // current_thread runtime: the lane cannot run until we await, so this is the whole storm.
        for i in 1..=100u64 {
            let e = epoch.advance();
            h.submit(utt(i, e.0, Prio::Interrupt, &format!("item {i}")));
        }
        h.notify_epoch();
        assert!(eventually(WAIT, || !ctl.cmds().is_empty()).await);
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(speaks(&ctl), [100]);
        let s = h.stats();
        assert_eq!(s.spoken, 1);
        assert_eq!(s.superseded, 99);
    }

    #[tokio::test]
    async fn a_newer_focus_replaces_the_running_one_in_a_single_command() {
        let (h, ctl, epoch, _t) = start(true, ClientHealth::Ready);
        let e = epoch.advance();
        h.submit(utt(1, e.0, Prio::Interrupt, "one"));
        assert!(eventually(WAIT, || !ctl.cmds().is_empty()).await);
        ctl.send(SpeechEvent::Started(UttId(1)));
        let e = epoch.advance();
        h.submit(utt(2, e.0, Prio::Interrupt, "two"));
        assert!(eventually(WAIT, || ctl.cmds().len() == 2).await);
        match &ctl.cmds()[1] {
            SpeechCmd::Replace { stop, then } => {
                assert_eq!(*stop, UttId(1));
                assert!(matches!(&**then, SpeechCmd::Speak { id, .. } if *id == UttId(2)));
            }
            other => panic!("expected one Replace, got {other:?}"),
        }
        assert_eq!(h.stats().replaced, 1);
    }

    #[tokio::test]
    async fn voice_params_reach_the_client_before_the_utterance_that_follows() {
        let (h, ctl, epoch, _t) = start(true, ClientHealth::Ready);
        let p = VoiceParams {
            rate: 10,
            pitch: 0,
            volume: 100,
        };
        // Queued together before the lane runs: the control still goes first.
        h.control(SpeechControl::SetParams(p));
        let e = epoch.advance();
        h.submit(utt(1, e.0, Prio::Interrupt, "rate 55"));
        assert!(eventually(WAIT, || ctl.cmds().len() >= 2).await);
        let cmds = ctl.cmds();
        assert_eq!(cmds[0], SpeechCmd::SetParams(p));
        assert!(matches!(&cmds[1], SpeechCmd::Speak { .. }));
    }

    #[tokio::test]
    async fn mute_drops_new_utterances_but_not_the_one_already_submitted() {
        let (h, ctl, epoch, _t) = start(true, ClientHealth::Ready);
        let e = epoch.advance();
        h.submit(utt(1, e.0, Prio::Interrupt, "speech off"));
        h.control(SpeechControl::Mute(true));
        h.submit(utt(2, e.0, Prio::Interrupt, "hidden"));
        assert!(eventually(WAIT, || !ctl.cmds().is_empty()).await);
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(speaks(&ctl), [1]);
        assert_eq!(h.stats().muted_dropped, 1);
        h.control(SpeechControl::Mute(false));
        let e = epoch.advance();
        h.submit(utt(3, e.0, Prio::Interrupt, "speech on"));
        assert!(eventually(WAIT, || speaks(&ctl).contains(&3)).await);
    }

    #[tokio::test]
    async fn pause_and_resume_reach_the_client() {
        let (h, ctl, epoch, _t) = start(true, ClientHealth::Ready);
        let e = epoch.advance();
        h.submit(utt(1, e.0, Prio::Interrupt, "long text"));
        assert!(eventually(WAIT, || !ctl.cmds().is_empty()).await);
        ctl.send(SpeechEvent::Started(UttId(1)));
        h.control(SpeechControl::Pause);
        assert!(eventually(WAIT, || ctl.cmds().contains(&SpeechCmd::Pause)).await);
        h.control(SpeechControl::Resume);
        assert!(eventually(WAIT, || ctl.cmds().contains(&SpeechCmd::Resume)).await);
    }

    #[tokio::test]
    async fn without_atomic_replace_it_is_stop_then_speak() {
        let (h, ctl, epoch, _t) = start(false, ClientHealth::Ready);
        let e = epoch.advance();
        h.submit(utt(1, e.0, Prio::Interrupt, "one"));
        assert!(eventually(WAIT, || !ctl.cmds().is_empty()).await);
        let e = epoch.advance();
        h.submit(utt(2, e.0, Prio::Interrupt, "two"));
        assert!(eventually(WAIT, || ctl.cmds().len() == 3).await);
        let c = ctl.cmds();
        assert!(matches!(c[1], SpeechCmd::Stop { id } if id == UttId(1)));
        assert!(matches!(&c[2], SpeechCmd::Speak { id, .. } if *id == UttId(2)));
    }

    #[tokio::test]
    async fn a_stale_utterance_is_never_sent() {
        let (h, ctl, epoch, _t) = start(true, ClientHealth::Ready);
        let e = epoch.advance();
        let late = utt(1, e.0, Prio::Interrupt, "late");
        epoch.advance(); // the user pressed a key before the lane ran
        h.submit(late);
        h.notify_epoch();
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert!(ctl.cmds().is_empty());
        assert_eq!(h.stats().stale_dropped, 1);
    }

    #[tokio::test]
    async fn a_key_press_silences_running_speech_without_a_new_utterance() {
        let (h, ctl, epoch, _t) = start(true, ClientHealth::Ready);
        let e = epoch.advance();
        h.submit(utt(1, e.0, Prio::Interrupt, "long text"));
        assert!(eventually(WAIT, || !ctl.cmds().is_empty()).await);
        ctl.send(SpeechEvent::Started(UttId(1)));
        epoch.advance();
        h.notify_epoch();
        assert!(eventually(WAIT, || ctl.cmds().len() == 2).await);
        assert!(matches!(ctl.cmds()[1], SpeechCmd::Stop { id } if id == UttId(1)));
    }

    #[tokio::test]
    async fn queued_utterances_follow_when_the_current_one_ends() {
        let (h, ctl, epoch, _t) = start(true, ClientHealth::Ready);
        let e = epoch.advance();
        h.submit(utt(1, e.0, Prio::Interrupt, "first"));
        assert!(eventually(WAIT, || !ctl.cmds().is_empty()).await);
        h.submit(utt(2, e.0, Prio::Text, "second"));
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(speaks(&ctl), [1], "same epoch: it waits its turn");
        ctl.send(SpeechEvent::Ended(UttId(1)));
        assert!(eventually(WAIT, || speaks(&ctl) == [1, 2]).await);
    }

    #[tokio::test]
    async fn nothing_is_sent_while_disconnected_and_the_latest_is_spoken_on_reconnect() {
        let (h, ctl, epoch, _t) = start(true, ClientHealth::Down);
        let e = epoch.advance();
        h.submit(utt(1, e.0, Prio::Interrupt, "queued"));
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert!(ctl.cmds().is_empty());
        ctl.set_health(ClientHealth::Ready);
        assert!(eventually(WAIT, || speaks(&ctl) == [1]).await);
    }

    #[tokio::test]
    async fn a_slow_replace_is_counted() {
        let (h, ctl, epoch, _t) = start(true, ClientHealth::Ready);
        let e = epoch.advance();
        h.submit(utt(1, e.0, Prio::Interrupt, "one"));
        assert!(eventually(WAIT, || !ctl.cmds().is_empty()).await);
        let e = epoch.advance();
        h.submit(utt(2, e.0, Prio::Interrupt, "two"));
        // No Started and no Ended(1) ever arrives.
        assert!(eventually(WAIT, || h.stats().replace_slow == 1).await);
    }

    #[tokio::test]
    async fn the_inbox_is_bounded() {
        let (h, _ctl, epoch, _t) = start(true, ClientHealth::Ready);
        let e = epoch.advance();
        h.submit(utt(1, e.0, Prio::Interrupt, "x"));
        h.shared.inflight_epoch.store(e.0, Ordering::Release);
        for i in 2..200 {
            h.submit(utt(i, e.0, Prio::Notification, "n"));
        }
        assert!(h.stats().inbox_dropped >= 150);
    }
}
