//! The speech lane (ADR-0001 sections 7.4, 8.1, 8.3): turns utterances into `SpeechCmd`s.
//!
//! Producers (the Reader, `PresentJob`s) call [`SpeechHandle::submit`], which never blocks:
//!
//! - an interrupting utterance (priority `Interrupt`, or an epoch newer than what is being
//!   spoken) goes into a **single slot**; a newer one replaces an older one that the lane has
//!   not picked up yet, so a focus storm leaves only the last focus to be spoken. The one
//!   exception is a notice (`Message` that survives focus changes): speech of another class
//!   takes the slot and the notice is queued right behind it instead of being overwritten;
//! - everything else goes into a bounded FIFO (32), dropping the newest when full.
//!
//! After advancing the `EpochCell` (a key press, a mode change) the producer calls
//! [`SpeechHandle::notify_epoch`]. The lane then runs the pure [`SpeechQueue`] state machine,
//! re-checks E-2 (`Utterance::is_valid`) when it is about to send, and turns effects into
//! commands for the client. Cancel + speak is one `SpeechCmd::Replace` (one write on a
//! backend with atomic replace).

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use gorca_model::{Epoch, Prio, UttId, Utterance};
use gorca_output_api::{
    ClientHealth, SpeechClient, SpeechCmd, SpeechEvent, SubmitError, VoiceParams, VoiceRef,
};
use gorca_rt::{EpochCell, Notify, TaskSet, mpsc};

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

/// What happened to an utterance, as the server reported it (ADR-0007 SA-3). Say-all uses it to
/// place its position and to keep the look-ahead full.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum LaneProgress {
    Started(UttId),
    Ended(UttId),
    /// Cancelled, replaced or failed: it will not end normally.
    Stopped(UttId),
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
    /// Utterances dropped because their epoch was stale (E-2): at submit, when the lane picked them
    /// up, or at send time.
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
    /// Where `Started`/`Ended`/`Stopped` of tracked utterances go, when someone listens. Never
    /// blocks: a full channel drops the message (the listener's watchdog covers it).
    progress: Mutex<Option<mpsc::Sender<LaneProgress>>>,
    /// Utterances submitted with [`SpeechHandle::submit_say_all`]: the only ones that report.
    tracked: Mutex<HashSet<UttId>>,
}

/// Tracked utterances kept at once: a reading holds three; this only bounds a leak.
const MAX_TRACKED: usize = 256;

impl LaneShared {
    /// Report `p` if its utterance is tracked; the end of its life forgets it.
    fn report(&self, p: LaneProgress) {
        let (id, last) = match p {
            LaneProgress::Started(id) => (id, false),
            LaneProgress::Ended(id) | LaneProgress::Stopped(id) => (id, true),
        };
        {
            let mut t = lock(&self.tracked);
            if !t.contains(&id) {
                return;
            }
            if last {
                t.remove(&id);
            }
        }
        if let Some(tx) = lock(&self.progress).as_ref() {
            let _ = tx.try_send(p);
        }
    }

    fn dropped(&self, id: UttId) {
        self.report(LaneProgress::Stopped(id));
    }
}

/// A notice that must not be lost to speech of another class: priority `Message` that
/// survives focus changes (the daemon's notices, `Reader::announce`). Ordinary `Message`
/// presentations, key echoes and confirmations stay latest-wins.
fn is_protected(u: &Utterance) -> bool {
    u.prio == Prio::Message && u.survives_focus_change
}

impl LaneShared {
    /// Put `u` at the front of the FIFO. A full FIFO drops it, counted.
    fn queue_front(&self, u: Utterance) {
        let mut q = lock(&self.inbox);
        if q.len() >= FIFO_CAPACITY {
            drop(q);
            self.stats.inbox_dropped.fetch_add(1, Ordering::Relaxed);
            self.dropped(u.id);
            return;
        }
        q.push_front(u);
    }
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
            s.dropped(u.id);
            return;
        }
        if !u.is_valid(s.epoch.current()) {
            s.stats.stale_dropped.fetch_add(1, Ordering::Relaxed);
            s.dropped(u.id);
            return;
        }
        // A notification never takes the single interrupting slot (M4b): it would replace a
        // focus utterance the lane has not picked up yet. It queues behind it.
        let interrupts = u.prio != Prio::Notification
            && (u.prio == Prio::Interrupt || u.epoch.0 > s.inflight_epoch.load(Ordering::Acquire));
        if interrupts {
            let mut slot = lock(&s.slot);
            match slot.take() {
                Some(old) if old.epoch > u.epoch => {
                    // An older interrupt arrived late: the one in the slot is newer.
                    *slot = Some(old);
                    drop(slot);
                    if is_protected(&u) {
                        // A notice survives its epoch: it follows what is in the slot.
                        s.queue_front(u);
                        s.wake.notify_one();
                    } else {
                        s.stats.stale_dropped.fetch_add(1, Ordering::Relaxed);
                        s.dropped(u.id);
                    }
                    return;
                }
                Some(old) if is_protected(&old) && u.prio != Prio::Message => {
                    // A notice is not displaced by speech of another class (a key echo, a
                    // confirmation): the newcomer takes the slot and speaks at once, the
                    // notice is queued right behind it.
                    *slot = Some(u);
                    drop(slot);
                    s.queue_front(old);
                    s.wake.notify_one();
                    return;
                }
                Some(old) => {
                    s.stats.superseded.fetch_add(1, Ordering::Relaxed);
                    s.dropped(old.id);
                }
                None => {}
            }
            *slot = Some(u);
        } else {
            let mut q = lock(&s.inbox);
            if q.len() >= FIFO_CAPACITY {
                s.stats.inbox_dropped.fetch_add(1, Ordering::Relaxed);
                s.dropped(u.id);
                return;
            }
            q.push_back(u);
        }
        s.wake.notify_one();
    }

    /// Queue a chunk of a reading (say-all, ADR-0007 SA-2) and report its progress.
    ///
    /// The `first` chunk is submitted like any utterance, so it interrupts what is being
    /// spoken. The others are **queued**: they never take the interrupting slot, so they cannot
    /// replace the first chunk or each other, whether the server is up or down, and ordinary
    /// speech keeps newest-wins. Every way the lane can lose a tracked utterance (replaced,
    /// stale, dropped, muted, discarded) is reported as `Stopped`, so the reader's book-keeping
    /// cannot leak a phantom chunk.
    pub fn submit_say_all(&self, u: Utterance, first: bool) {
        let s = &self.shared;
        {
            let mut t = lock(&s.tracked);
            if t.len() >= MAX_TRACKED {
                t.clear();
            }
            t.insert(u.id);
        }
        if first {
            self.submit(u);
            return;
        }
        if s.muted.load(Ordering::Acquire) {
            s.stats.muted_dropped.fetch_add(1, Ordering::Relaxed);
            s.dropped(u.id);
            return;
        }
        if !u.is_valid(s.epoch.current()) {
            s.stats.stale_dropped.fetch_add(1, Ordering::Relaxed);
            s.dropped(u.id);
            return;
        }
        {
            let mut q = lock(&s.inbox);
            if q.len() >= FIFO_CAPACITY {
                s.stats.inbox_dropped.fetch_add(1, Ordering::Relaxed);
                s.dropped(u.id);
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

    /// Report the progress of the utterances submitted with [`Self::submit_say_all`] to `tx`.
    pub fn set_progress(&self, tx: mpsc::Sender<LaneProgress>) {
        *lock(&self.shared.progress) = Some(tx);
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
            progress: Mutex::new(None),
            tracked: Mutex::new(HashSet::new()),
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
            let tick_at = tick_at.or_else(|| self.queue.next_wake(Instant::now()));
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
            SpeechEvent::Started(id) => self.shared.report(LaneProgress::Started(id)),
            SpeechEvent::Ended(id) => self.shared.report(LaneProgress::Ended(id)),
            SpeechEvent::Stopped(id) | SpeechEvent::Error(id, _) => {
                self.shared.dropped(id);
            }
            SpeechEvent::Accepted(_) | SpeechEvent::Mark(..) | SpeechEvent::AudioProgress(..) => {}
        }
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
        } else if matches!(self.queue.state(), SpeechState::Idle) && self.queue.queued() > 0 {
            // Something waited (a notification in its grace): start it if it may start now.
            self.feed(SpeechMsg::Tick);
        }
        // Utterances that were never handed on and are not tracked any more must not pile up.
        if self.store.len() > 4 * FIFO_CAPACITY {
            let lost: Vec<UttId> = self.store.keys().copied().collect();
            self.store.clear();
            for id in lost {
                self.shared.dropped(id);
            }
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
            self.shared.dropped(id);
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
                    self.shared.dropped(u.id);
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
                    self.shared.dropped(u.id);
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
                self.shared.dropped(stop);
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
                self.shared.dropped(id);
                self.send(SpeechCmd::Stop { id }, None);
            }
            SpeechEffect::StopAll => self.send(SpeechCmd::StopAll, None),
            SpeechEffect::Pause => self.send(SpeechCmd::Pause, None),
            SpeechEffect::Resume => self.send(SpeechCmd::Resume, None),
            SpeechEffect::Discard(id) => {
                // The state machine drops an utterance that was valid at submit but is stale
                // by the time the lane runs (E-2), or one the FIFO has no room for. Count it:
                // a silent drop made "submitted" and "spoken" disagree without a reason.
                match self.store.remove(&id) {
                    Some(u) if !u.is_valid(self.shared.epoch.current()) => {
                        self.shared
                            .stats
                            .stale_dropped
                            .fetch_add(1, Ordering::Relaxed);
                    }
                    Some(_) => {
                        self.shared
                            .stats
                            .inbox_dropped
                            .fetch_add(1, Ordering::Relaxed);
                    }
                    None => {}
                }
                self.shared.dropped(id);
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
            ttl_secs: 0,
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
    async fn ordinary_same_epoch_speech_keeps_newest_wins_in_the_slot() {
        let (h, ctl, epoch, _t) = start(true, ClientHealth::Ready);
        let e = epoch.advance();
        for i in 1..=3u64 {
            h.submit(utt(i, e.0, Prio::Text, &format!("item {i}")));
        }
        assert!(eventually(WAIT, || !ctl.cmds().is_empty()).await);
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(speaks(&ctl), [3]);
    }

    /// A handle with no lane behind it: the slot and the inbox stay where the producer left them.
    fn detached() -> (SpeechHandle, Arc<EpochCell>) {
        let epoch = Arc::new(EpochCell::new());
        let shared = Arc::new(LaneShared {
            slot: Mutex::new(None),
            inbox: Mutex::new(VecDeque::new()),
            controls: Mutex::new(VecDeque::new()),
            muted: AtomicBool::new(false),
            wake: Notify::new(),
            epoch: Arc::clone(&epoch),
            inflight_epoch: AtomicU64::new(0),
            stats: Counters::default(),
            progress: Mutex::new(None),
            tracked: Mutex::new(HashSet::new()),
        });
        (SpeechHandle { shared }, epoch)
    }

    fn slot_id(h: &SpeechHandle) -> Option<u64> {
        lock(&h.shared.slot).as_ref().map(|u| u.id.0)
    }

    fn inbox_ids(h: &SpeechHandle) -> Vec<u64> {
        lock(&h.shared.inbox).iter().map(|u| u.id.0).collect()
    }

    fn notice(id: u64, epoch: u64, text: &str) -> Utterance {
        let mut u = utt(id, epoch, Prio::Message, text);
        u.survives_focus_change = true;
        u
    }

    #[test]
    fn a_pending_notice_is_not_overwritten_by_a_key_echo() {
        let (h, epoch) = detached();
        let e = epoch.advance();
        h.submit(notice(1, e.0, "application not responding"));
        h.submit(utt(2, e.0, Prio::Interrupt, "a"));
        // The echo owns the slot (spoken at once), the notice waits right behind it.
        assert_eq!((slot_id(&h), inbox_ids(&h)), (Some(2), vec![1]));
        assert_eq!(h.stats().superseded, 0);
    }

    #[test]
    fn a_notice_goes_ahead_of_queued_speech_and_survives_a_newer_epoch() {
        let (h, epoch) = detached();
        let e = epoch.advance();
        h.submit_say_all(utt(9, e.0, Prio::Text, "queued"), false);
        h.submit(notice(1, e.0, "insert stuck"));
        let e2 = epoch.advance();
        h.submit(utt(2, e2.0, Prio::Interrupt, "b"));
        assert_eq!((slot_id(&h), inbox_ids(&h)), (Some(2), vec![1, 9]));
        assert_eq!(h.stats().superseded, 0);
    }

    #[test]
    fn a_late_notice_follows_a_newer_interrupt_instead_of_being_dropped() {
        let (h, epoch) = detached();
        let e = epoch.advance();
        let e2 = epoch.advance();
        h.submit(utt(2, e2.0, Prio::Interrupt, "newer"));
        h.submit(notice(1, e.0, "late notice"));
        assert_eq!((slot_id(&h), inbox_ids(&h)), (Some(2), vec![1]));
        assert_eq!(h.stats().stale_dropped, 0);
        // An ordinary late utterance is still dropped.
        h.submit(utt(3, e.0, Prio::Interrupt, "late echo"));
        assert_eq!(h.stats().stale_dropped, 1);
    }

    #[test]
    fn same_class_notices_and_ordinary_messages_stay_latest_wins() {
        let (h, epoch) = detached();
        let e = epoch.advance();
        h.submit(notice(1, e.0, "one"));
        h.submit(notice(2, e.0, "two"));
        h.submit(utt(3, e.0, Prio::Message, "plain"));
        h.submit(utt(4, e.0, Prio::Interrupt, "echo"));
        assert_eq!((slot_id(&h), inbox_ids(&h)), (Some(4), vec![]));
        assert_eq!(h.stats().superseded, 3);
    }

    #[test]
    fn a_full_fifo_drops_the_displaced_notice_counted() {
        let (h, epoch) = detached();
        let e = epoch.advance();
        for i in 0..FIFO_CAPACITY as u64 {
            h.submit_say_all(utt(100 + i, e.0, Prio::Text, "fill"), false);
        }
        h.submit(notice(1, e.0, "n"));
        h.submit(utt(2, e.0, Prio::Interrupt, "echo"));
        let s = h.stats();
        assert_eq!((s.inbox_dropped, s.superseded), (1, 0));
        assert_eq!(slot_id(&h), Some(2));
    }

    #[tokio::test]
    async fn a_notice_and_a_key_echo_are_both_accounted_for_end_to_end() {
        let (h, ctl, epoch, _t) = start(true, ClientHealth::Ready);
        let e = epoch.advance();
        h.submit(notice(1, e.0, "notice"));
        h.submit(utt(2, e.0, Prio::Interrupt, "a"));
        assert!(eventually(WAIT, || speaks(&ctl).contains(&2)).await);
        let s = h.stats();
        // Never a silent loss: nothing is superseded, both reach the server or are counted.
        assert_eq!(s.superseded, 0);
        assert!(s.spoken >= 1);
    }

    #[tokio::test]
    async fn say_all_chunks_queue_behind_the_first_instead_of_replacing_it() {
        let (h, ctl, epoch, _t) = start(true, ClientHealth::Ready);
        let e = epoch.advance();
        h.submit_say_all(utt(1, e.0, Prio::Text, "chunk 1"), true);
        h.submit_say_all(utt(2, e.0, Prio::Text, "chunk 2"), false);
        h.submit_say_all(utt(3, e.0, Prio::Text, "chunk 3"), false);
        assert!(eventually(WAIT, || speaks(&ctl) == [1]).await);
        ctl.send(SpeechEvent::Started(UttId(1)));
        ctl.send(SpeechEvent::Ended(UttId(1)));
        assert!(eventually(WAIT, || speaks(&ctl) == [1, 2]).await);
        ctl.send(SpeechEvent::Ended(UttId(2)));
        assert!(eventually(WAIT, || speaks(&ctl) == [1, 2, 3]).await);
    }

    /// Probe P5 of the M3 review: the speech server is down when say-all starts.
    #[tokio::test]
    async fn say_all_chunks_all_reach_the_server_after_it_was_down_at_the_start() {
        let (h, ctl, epoch, _t) = start(true, ClientHealth::Down);
        let (tx, mut rx) = gorca_rt::mpsc::channel(64);
        h.set_progress(tx);
        let e = epoch.advance();
        h.submit_say_all(utt(1, e.0, Prio::Text, "chunk 1"), true);
        tokio::time::sleep(Duration::from_millis(150)).await;
        h.submit_say_all(utt(2, e.0, Prio::Text, "chunk 2"), false);
        h.submit_say_all(utt(3, e.0, Prio::Text, "chunk 3"), false);
        tokio::time::sleep(Duration::from_millis(30)).await;
        ctl.set_health(ClientHealth::Ready);
        for want in 1..=3u64 {
            assert!(eventually(WAIT, || speaks(&ctl).len() as u64 >= want).await);
            ctl.send(SpeechEvent::Started(UttId(want)));
            ctl.send(SpeechEvent::Ended(UttId(want)));
        }
        assert_eq!(
            speaks(&ctl),
            [1, 2, 3],
            "every chunk reaches the server, in order"
        );
        assert_eq!(h.stats().superseded, 0);
        let mut got = Vec::new();
        for _ in 0..6 {
            if let Ok(Some(p)) = tokio::time::timeout(WAIT, rx.recv()).await {
                got.push(p);
            }
        }
        assert_eq!(got.len(), 6, "{got:?}");
    }

    #[tokio::test]
    async fn only_say_all_chunks_report_progress() {
        let (h, ctl, epoch, _t) = start(true, ClientHealth::Ready);
        let (tx, mut rx) = gorca_rt::mpsc::channel(8);
        h.set_progress(tx);
        let e = epoch.advance();
        h.submit(utt(1, e.0, Prio::Interrupt, "an echo"));
        assert!(eventually(WAIT, || !ctl.cmds().is_empty()).await);
        ctl.send(SpeechEvent::Started(UttId(1)));
        ctl.send(SpeechEvent::Ended(UttId(1)));
        let e = epoch.advance();
        h.submit_say_all(utt(2, e.0, Prio::Text, "chunk"), true);
        assert!(eventually(WAIT, || speaks(&ctl) == [1, 2]).await);
        ctl.send(SpeechEvent::Started(UttId(2)));
        ctl.send(SpeechEvent::Ended(UttId(2)));
        ctl.send(SpeechEvent::Stopped(UttId(9)));
        let mut got = Vec::new();
        for _ in 0..2 {
            if let Ok(Some(p)) = tokio::time::timeout(WAIT, rx.recv()).await {
                got.push(p);
            }
        }
        assert_eq!(
            got,
            [
                LaneProgress::Started(UttId(2)),
                LaneProgress::Ended(UttId(2))
            ]
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(50), rx.recv())
                .await
                .is_err(),
            "nothing else reports"
        );
    }

    #[tokio::test]
    async fn a_replaced_or_stale_chunk_is_reported_stopped() {
        let (h, _ctl, epoch, _t) = start(true, ClientHealth::Down);
        let (tx, mut rx) = gorca_rt::mpsc::channel(8);
        h.set_progress(tx);
        let e = epoch.advance();
        // Two interrupting submissions in one slot: the first is replaced.
        h.submit_say_all(utt(1, e.0, Prio::Text, "one"), true);
        h.submit(utt(2, e.0, Prio::Interrupt, "two"));
        assert_eq!(
            tokio::time::timeout(WAIT, rx.recv()).await.ok().flatten(),
            Some(LaneProgress::Stopped(UttId(1)))
        );
        // A chunk of an epoch that is already over is dropped at once.
        let _ = epoch.advance();
        h.submit_say_all(utt(3, e.0, Prio::Text, "stale"), false);
        assert_eq!(
            tokio::time::timeout(WAIT, rx.recv()).await.ok().flatten(),
            Some(LaneProgress::Stopped(UttId(3)))
        );
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

    /// Bug A of the M3 E2 flake: an utterance that is valid at `submit` and made stale by the next
    /// key before the lane runs was dropped by the state machine without a trace, so the
    /// utterances submitted and the ones spoken disagreed with no counter to explain it.
    #[tokio::test]
    async fn an_utterance_made_stale_after_submit_is_counted_not_lost_silently() {
        let (h, ctl, epoch, _t) = start(true, ClientHealth::Ready);
        let e = epoch.advance();
        // current_thread runtime: the lane cannot run between these lines.
        h.submit(utt(1, e.0, Prio::Interrupt, "echo of the first key"));
        epoch.advance(); // the next key arrives before the lane picked the echo up
        h.notify_epoch();
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert!(ctl.cmds().is_empty(), "newest wins: nothing stale is sent");
        let s = h.stats();
        assert_eq!(s.stale_dropped, 1, "the drop is accounted for");
        assert_eq!(s.spoken, 0);
        // Every submitted utterance is spoken or counted.
        let e = epoch.advance();
        h.submit(utt(2, e.0, Prio::Interrupt, "second"));
        assert!(eventually(WAIT, || speaks(&ctl) == [2]).await);
        let s = h.stats();
        assert_eq!(s.spoken + s.stale_dropped + s.superseded, 2);
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
    async fn a_notification_never_replaces_a_pending_focus_utterance() {
        // Disconnected: both stay with the lane (focus in the slot, the notification in the
        // FIFO) until the server is back; the focus must be spoken first and not be replaced.
        let (h, ctl, epoch, _t) = start(true, ClientHealth::Down);
        let e = epoch.advance();
        h.submit(utt(1, e.0, Prio::Interrupt, "focus"));
        h.submit(utt(2, e.0, Prio::Notification, "note"));
        assert_eq!(h.stats().superseded, 0, "the notification took no slot");
        ctl.set_health(ClientHealth::Ready);
        assert!(eventually(WAIT, || speaks(&ctl) == [1]).await);
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(
            speaks(&ctl),
            [1],
            "the notification waits for the focus speech"
        );
        ctl.send(SpeechEvent::Ended(UttId(1)));
        assert!(eventually(WAIT, || speaks(&ctl) == [1, 2]).await);
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
