//! `KeySource` for Mutter's `org.freedesktop.a11y.KeyboardMonitor` (ADR-0001 sections 5.5, 7.3,
//! ADR-0005 4, 8.2).
//!
//! One session per D-Bus connection: own `org.gnome.Orca.KeyboardMonitor` (ID-1) **without ever
//! taking it from a running Orca**, subscribe to the unicast `KeyEvent` signal, call
//! `WatchKeyboard`, then drive the pure [`MonitorState`] machine: the grab set the Reader wants
//! becomes `SetKeyGrabs` (with the x4 lock-mask variants, K-6), settles for [`SETTLE`], and is
//! published to the input lane as a [`GrabView`] (committed and wanted, so keys in flight are
//! classified `Ambiguous` instead of guessed).
//!
//! Every D-Bus call has a deadline (K-4) and runs in a task owned by the session. A lost
//! manager (shell restart), a lost name or a dropped connection ends the session; the supervisor
//! loop retries within about a second and the grabs are re-sent. Notices go to the user once.

use std::sync::Arc;
use std::time::Duration;

use futures_lite::StreamExt;
use gorca_dbus::{DbusError, MAX_QUEUED, call_method, with_deadline};
use gorca_input_api::{
    GrabSet, GrabView, KeyError, KeyEvent, KeySource, KeySourceCtx, KeySourceKind, LaneLinks,
    Modifiers,
};
use gorca_rt::{BoxFuture, BudgetClass, Deadline, mpsc};
use tokio::task::JoinSet;

use crate::monitor::{
    GRAB_DEADLINE, GrabSetState, MonitorEffect, MonitorMsg, MonitorState, Notice, SETTLE, StepCtx,
};
use crate::{MANAGER_NAME, MANAGER_PATH, MONITOR_INTERFACE, ORCA_MONITOR_NAME};

/// `DBUS_NAME_FLAG_DO_NOT_QUEUE`: never wait in line behind a running Orca.
const DO_NOT_QUEUE: u32 = 4;
const BACKOFF_MIN: Duration = Duration::from_millis(100);
/// Short on purpose: after a shell restart the keyboard must be back within about two seconds.
const BACKOFF_MAX: Duration = Duration::from_secs(1);
/// While another screen reader holds the name, look again this often.
const HELD_RETRY: Duration = Duration::from_secs(5);
/// How often the held-modifier watchdog looks, and how long a "held" modifier may be silent.
const WATCHDOG_TICK: Duration = Duration::from_secs(1);
const HELD_QUIET: Duration = Duration::from_secs(5);
/// Results of in-flight calls waiting for the session loop.
const CALL_RESULTS: usize = 16;

/// `RequestName` replies.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum NameReply {
    Primary,
    InQueue,
    Exists,
    AlreadyOwner,
    Other(u32),
}

impl From<u32> for NameReply {
    fn from(v: u32) -> Self {
        match v {
            1 => NameReply::Primary,
            2 => NameReply::InQueue,
            3 => NameReply::Exists,
            4 => NameReply::AlreadyOwner,
            other => NameReply::Other(other),
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum Attempt {
    #[error("{0}")]
    Dbus(#[from] DbusError),
    #[error("another screen reader owns {ORCA_MONITOR_NAME}")]
    Held,
    #[error("bad reply: {0}")]
    Reply(String),
    #[error("the key stream ended")]
    StreamEnded,
    #[error("the keyboard manager restarted")]
    ManagerGone,
    #[error("the monitor name was lost")]
    NameLost,
    #[error("the grab set could not be applied")]
    GrabsFailed,
}

/// The `KeyEvent` signal body `(b u u u q)`: released, state, keysym, unichar, keycode.
pub fn decode_key_event(body: (bool, u32, u32, u32, u16)) -> KeyEvent {
    let (released, state, keysym, unichar, keycode) = body;
    KeyEvent {
        keycode,
        keysym,
        unichar,
        modifiers: Modifiers::from_bits_truncate(state),
        pressed: !released,
    }
}

#[derive(Clone, Debug, Default)]
pub struct MutterKeySource {
    /// Session bus address, fully resolved by the caller (this crate reads no environment).
    pub session_address: String,
    /// Where the grab sets are published for the input lane.
    links: Option<Arc<LaneLinks>>,
    /// What the user must hear (the daemon forwards it to the Reader).
    notices: Option<mpsc::Sender<Notice>>,
    settle: Option<Duration>,
}

impl MutterKeySource {
    pub fn new(session_address: String) -> Self {
        Self {
            session_address,
            ..Self::default()
        }
    }

    #[must_use]
    pub fn with_links(mut self, links: Arc<LaneLinks>) -> Self {
        self.links = Some(links);
        self
    }

    #[must_use]
    pub fn with_notices(mut self, tx: mpsc::Sender<Notice>) -> Self {
        self.notices = Some(tx);
        self
    }

    /// Override the grab settle time (P-7: 2x the measured p99.9 if 5 ms proves too short).
    #[must_use]
    pub fn with_settle(mut self, settle: Duration) -> Self {
        self.settle = Some(settle);
        self
    }

    fn notify(&self, n: Notice) {
        if let Some(tx) = &self.notices
            && tx.try_send(n).is_err()
        {
            tracing::warn!(notice = ?n, "notice queue full; dropped a notice");
        }
    }
}

impl KeySource for MutterKeySource {
    fn kind(&self) -> KeySourceKind {
        KeySourceKind::Mutter
    }

    fn run(self: Box<Self>, ctx: KeySourceCtx) -> BoxFuture<'static, Result<(), KeyError>> {
        Box::pin(async move {
            let mut backoff = BACKOFF_MIN;
            let mut told_lost = false;
            let mut told_held = false;
            loop {
                let started = tokio::time::Instant::now();
                let mut watching = false;
                let err = tokio::select! {
                    () = ctx.shutdown.cancelled() => return Ok(()),
                    r = session(&self, &ctx, &mut told_lost, &mut watching) => r,
                };
                if watching {
                    told_held = false;
                    if let Some(l) = &self.links {
                        l.session.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                    }
                }
                if watching && !told_lost {
                    // The keyboard was working and is gone: say so once, not on every retry.
                    told_lost = true;
                    self.notify(Notice::KeyboardCommandsUnavailable);
                }
                let wait = match err {
                    Err(Attempt::Held) => {
                        tracing::info!(
                            "another screen reader holds the keyboard monitor; waiting for it"
                        );
                        if !told_held {
                            told_held = true;
                            self.notify(Notice::AnotherScreenReaderHoldsKeyboard);
                        }
                        HELD_RETRY
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, ?backoff, "keyboard monitor lost; retrying");
                        backoff
                    }
                    Ok(()) => return Ok(()),
                };
                tokio::select! {
                    () = ctx.shutdown.cancelled() => return Ok(()),
                    () = tokio::time::sleep(wait) => {}
                }
                backoff = if started.elapsed() > Duration::from_secs(30) {
                    BACKOFF_MIN
                } else {
                    (backoff * 2).min(BACKOFF_MAX)
                };
            }
        })
    }
}

/// A timer the session loop waits for.
#[derive(Copy, Clone, Debug)]
enum Timer {
    Settle { generation: u32 },
    Retry,
}

/// The bus daemon's own name: the only legitimate sender of `NameOwnerChanged`.
const BUS_DAEMON: &str = "org.freedesktop.DBus";

/// Where a message claims to come from, and therefore who may have sent it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Origin {
    /// The Mutter a11y manager: only the unique name that owns [`MANAGER_NAME`] right now.
    Manager,
    /// The bus daemon: only [`BUS_DAEMON`].
    BusDaemon,
}

/// The one gate every inbound message passes before it can change anything.
///
/// Signals can be unicast to us by any client on the bus, and a unicast signal ignores the match
/// rule, so the rule's `sender=` proves nothing: the header is checked here, together with the
/// message type, interface, member and path. A forged `KeyEvent` must not type or run commands,
/// and a forged `NameOwnerChanged` must neither restart the session (a denial of service) nor
/// move the owner we trust. Until the manager's owner is known nothing from it is believed.
#[derive(Debug, Default)]
pub(crate) struct Gate {
    manager: Option<String>,
    rejected: u64,
}

impl Gate {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// The unique name that owns the manager's well-known name (resolved after subscribing).
    pub(crate) fn set_manager(&mut self, owner: String) {
        self.manager = Some(owner);
    }

    #[cfg(test)]
    pub(crate) fn rejected(&self) -> u64 {
        self.rejected
    }

    fn reject(&mut self, what: &str, sender: Option<&str>) {
        self.rejected += 1;
        if self.rejected.is_power_of_two() {
            // Rate limited: 1st, 2nd, 4th, 8th, ... A real manager that we mis-identified would
            // otherwise leave the commands dead without a trace.
            tracing::warn!(
                what,
                expected = ?self.manager,
                got = ?sender,
                dropped = self.rejected,
                "dropped a message that failed the sender gate"
            );
        }
    }

    /// May `msg` be believed as a signal from `origin` with this interface, member and path?
    pub(crate) fn admit(
        &mut self,
        origin: Origin,
        msg: &zbus::Message,
        interface: &str,
        member: &str,
        path: &str,
    ) -> bool {
        let h = msg.header();
        let sender = h.sender().map(zbus::names::UniqueName::as_str);
        let sender_ok = match origin {
            Origin::Manager => matches!((sender, &self.manager), (Some(s), Some(m)) if s == m),
            Origin::BusDaemon => sender == Some(BUS_DAEMON),
        };
        let shape_ok = msg.message_type() == zbus::message::Type::Signal
            && h.interface().is_some_and(|i| i.as_str() == interface)
            && h.member().is_some_and(|m| m.as_str() == member)
            && h.path().is_some_and(|p| p.as_str() == path);
        if sender_ok && shape_ok {
            true
        } else {
            self.reject(member, sender);
            false
        }
    }

    /// A `KeyEvent` from the manager, or `None` (forged, malformed or another signal).
    pub(crate) fn key_event(&mut self, msg: &zbus::Message) -> Option<KeyEvent> {
        if !self.admit(
            Origin::Manager,
            msg,
            MONITOR_INTERFACE,
            "KeyEvent",
            MANAGER_PATH,
        ) {
            return None;
        }
        msg.body()
            .deserialize::<(bool, u32, u32, u32, u16)>()
            .ok()
            .map(decode_key_event)
    }

    /// `(old, new)` of a `NameOwnerChanged` about `name`, only if the bus daemon sent it.
    pub(crate) fn owner_change(
        &mut self,
        msg: &zbus::Message,
        name: &str,
    ) -> Option<(String, String)> {
        if !self.admit(
            Origin::BusDaemon,
            msg,
            BUS_DAEMON,
            "NameOwnerChanged",
            "/org/freedesktop/DBus",
        ) {
            return None;
        }
        let (n, old, new) = msg.body().deserialize::<(String, String, String)>().ok()?;
        (n == name).then_some((old, new))
    }
}

/// Has the owner of `name` changed, as far as the stream has delivered? Looks at what is ready
/// and never waits. Forged items are skipped; a closed stream counts as a change (restart).
async fn owner_moved(gate: &mut Gate, stream: &mut zbus::MessageStream, name: &str) -> bool {
    while let Some(item) = futures_lite::future::poll_once(stream.next()).await {
        match item {
            None => return true,
            Some(Err(_)) => {}
            Some(Ok(m)) => {
                if gate.owner_change(&m, name).is_some() {
                    return true;
                }
            }
        }
    }
    false
}

fn rule(
    sender: &str,
    interface: &str,
    member: &str,
    arg0: Option<&str>,
) -> Result<zbus::MatchRule<'static>, Attempt> {
    let b = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender(sender.to_owned())
        .map_err(DbusError::Zbus)?
        .interface(interface.to_owned())
        .and_then(|b| b.member(member.to_owned()))
        .map_err(DbusError::Zbus)?;
    let b = match arg0 {
        Some(a) => b.arg(0, a.to_owned()).map_err(DbusError::Zbus)?,
        None => b,
    };
    Ok(b.build())
}

async fn session(
    src: &MutterKeySource,
    ctx: &KeySourceCtx,
    told_lost: &mut bool,
    watching: &mut bool,
) -> Result<(), Attempt> {
    let deadline = Deadline::for_class(&ctx.clock, BudgetClass::Startup);
    gorca_rt::guard::check(&src.session_address);
    let builder = zbus::connection::Builder::address(src.session_address.as_str())
        .map_err(DbusError::Zbus)?
        .max_queued(MAX_QUEUED)
        .method_timeout(Duration::from_secs(2));
    let conn = with_deadline(deadline, builder.build()).await?;

    // ID-1: ask for the name without queueing, so a running Orca is never displaced.
    let deadline = Deadline::for_class(&ctx.clock, BudgetClass::Startup);
    let msg = call_method(
        &conn,
        deadline,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        "RequestName",
        &(ORCA_MONITOR_NAME, DO_NOT_QUEUE),
    )
    .await?;
    let reply: u32 = msg
        .body()
        .deserialize()
        .map_err(|e| Attempt::Reply(e.to_string()))?;
    match NameReply::from(reply) {
        NameReply::Primary | NameReply::AlreadyOwner => {}
        NameReply::InQueue | NameReply::Exists => return Err(Attempt::Held),
        NameReply::Other(v) => return Err(Attempt::Reply(format!("RequestName returned {v}"))),
    }

    let mut gate = Gate::new();
    // Subscribe before asking for events: KeyEvent is unicast with no acknowledgement (F-2).
    let deadline = Deadline::for_class(&ctx.clock, BudgetClass::Startup);
    let mut keys = with_deadline(
        deadline,
        zbus::MessageStream::for_match_rule(
            rule(MANAGER_NAME, MONITOR_INTERFACE, "KeyEvent", None)?,
            &conn,
            Some(MAX_QUEUED),
        ),
    )
    .await?;
    // The manager restarting or our name changing hands both end the session.
    let mut owners = with_deadline(
        deadline,
        zbus::MessageStream::for_match_rule(
            rule(
                BUS_DAEMON,
                BUS_DAEMON,
                "NameOwnerChanged",
                Some(MANAGER_NAME),
            )?,
            &conn,
            Some(64),
        ),
    )
    .await?;
    let mut own_name = with_deadline(
        deadline,
        zbus::MessageStream::for_match_rule(
            rule(
                BUS_DAEMON,
                BUS_DAEMON,
                "NameOwnerChanged",
                Some(ORCA_MONITOR_NAME),
            )?,
            &conn,
            Some(64),
        ),
    )
    .await?;

    // Who is the manager right now? Everything else claiming to be it is dropped. This comes
    // after the owner subscriptions, so a replace in between is seen, never missed (F4).
    let deadline = Deadline::for_class(&ctx.clock, BudgetClass::Startup);
    let reply = call_method(
        &conn,
        deadline,
        BUS_DAEMON,
        "/org/freedesktop/DBus",
        BUS_DAEMON,
        "GetNameOwner",
        &(MANAGER_NAME,),
    )
    .await?;
    let manager_owner: String = reply
        .body()
        .deserialize()
        .map_err(|e| Attempt::Reply(e.to_string()))?;
    gate.set_manager(manager_owner);
    // An owner change that happened while we resolved: our answer may already be stale.
    if owner_moved(&mut gate, &mut owners, MANAGER_NAME).await {
        return Err(Attempt::ManagerGone);
    }
    let deadline = Deadline::for_class(&ctx.clock, BudgetClass::Startup);
    call_method(
        &conn,
        deadline,
        MANAGER_NAME,
        MANAGER_PATH,
        MONITOR_INTERFACE,
        "WatchKeyboard",
        &(),
    )
    .await?;
    if owner_moved(&mut gate, &mut owners, MANAGER_NAME).await {
        return Err(Attempt::ManagerGone);
    }
    tracing::info!("keyboard monitor active");
    *watching = true;
    if let Some(l) = &src.links {
        // F3: the lane forgets what it knew of the previous connection.
        l.session.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    }
    if *told_lost {
        *told_lost = false;
        src.notify(Notice::KeyboardRestored);
    }

    let now = std::time::Instant::now();
    let sctx = StepCtx { now };
    let (state, _) = MonitorState::start();
    // The name is ours and WatchKeyboard answered: the machine is Watching.
    let (state, _) = state.step(
        MonitorMsg::NameReply(crate::monitor::RequestNameReply::PrimaryOwner),
        &sctx,
    );
    let (mut state, effects) = state.step(MonitorMsg::WatchOk, &sctx);

    let (res_tx, mut results) = mpsc::channel::<MonitorMsg>(CALL_RESULTS);
    let mut calls: JoinSet<()> = JoinSet::new();
    let mut timers: Vec<(tokio::time::Instant, Timer)> = Vec::new();
    let mut grabs = ctx.grabs.clone();
    let settle = src.settle.unwrap_or(SETTLE);
    let mut published: Option<(u32, Option<u32>)> = None;
    // Grabbed modifiers that are down, and a grab set waiting for their release: changing the
    // modifier grabs while one is held leaves XKB with a stuck key (P-7).
    let mut held: Vec<u32> = Vec::new();
    let mut deferred: Option<GrabSet> = None;
    let mut last_key = tokio::time::Instant::now();
    let mut key_overflows = 0u64;
    let mut watchdog = tokio::time::interval(WATCHDOG_TICK);
    watchdog.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // F2: grab mutations are strictly ordered: one in flight, the newest wanted set queued.
    let mut writer = GrabWriter::default();

    let mut pending: Vec<MonitorEffect> = effects;
    loop {
        // Run the effects of the last step; each may produce further messages.
        while let Some(e) = pending.pop() {
            exec(
                e,
                &conn,
                &grabs,
                settle,
                &res_tx,
                &mut calls,
                &mut timers,
                &mut state,
                &mut pending,
                &mut writer,
                ctx,
            );
        }
        publish(&state, src.links.as_ref(), &mut published);
        if matches!(state, MonitorState::Lost { .. }) {
            return Err(Attempt::GrabsFailed);
        }
        let next_timer = timers.iter().map(|(t, _)| *t).min();
        // Biased: key events first. A grab change must see every key that was already
        // delivered, or it could change a modifier's grab while that modifier is down (F1).
        let msg: MonitorMsg = tokio::select! {
            biased;
            m = keys.next() => {
                let Some(m) = m else { return Err(Attempt::StreamEnded) };
                if let Ok(m) = m
                    && let Some(ev) = gate.key_event(&m)
                {
                    let may_be_modifiers = src
                        .links
                        .as_ref()
                        .map(|l| l.keymap.load().modifiers.clone())
                        .unwrap_or_default();
                    track_held(&mut held, &state, &may_be_modifiers, &ev);
                    last_key = tokio::time::Instant::now();
                    if !ctx.sink.push(ev) {
                        // The lane missed an event, so it no longer knows which keys are down
                        // (a dropped release leaves it stale for ever). Never carry on silently:
                        // make it forget. `held` above already saw the event, so the grab
                        // deferral stays exact.
                        key_overflows += 1;
                        tracing::warn!(key_overflows, "key channel full; resynchronising the input lane");
                        if let Some(l) = &src.links {
                            l.session.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                        }
                    }
                }
                match take_deferred(&mut deferred, &held) {
                    Some(set) => MonitorMsg::SetGrabSet(set),
                    None => continue,
                }
            }
            m = owners.next() => {
                // Any change of the manager's owner invalidates our grabs: start over. Only the
                // bus daemon's signal about the manager's name counts; a forged one is dropped.
                match m {
                    None => return Err(Attempt::StreamEnded),
                    Some(Ok(m)) if gate.owner_change(&m, MANAGER_NAME).is_some() => {
                        return Err(Attempt::ManagerGone);
                    }
                    Some(Ok(_) | Err(_)) => continue,
                }
            }
            m = own_name.next() => {
                match m {
                    None => return Err(Attempt::StreamEnded),
                    Some(Ok(m)) => {
                        // Our name lost: it now has no owner, or someone else.
                        if let Some((_, new)) = gate.owner_change(&m, ORCA_MONITOR_NAME)
                            && new.is_empty()
                        {
                            return Err(Attempt::NameLost);
                        }
                    }
                    Some(Err(_)) => {}
                }
                continue;
            }
            r = grabs.changed() => {
                if r.is_err() {
                    // The Reader side is gone: the daemon is shutting down.
                    return Ok(());
                }
                let want = grabs.borrow_and_update().clone();
                if !held.is_empty() && modifiers_change(&state, &want) {
                    deferred = Some(want);
                    continue;
                }
                deferred = None;
                MonitorMsg::SetGrabSet(want)
            }
            Some(m) = results.recv() => {
                if let Some((generation, set)) = writer.done() {
                    spawn_grab_call(&mut calls, &conn, &res_tx, &ctx.clock, generation, set);
                }
                m
            }
            () = sleep_until(next_timer), if next_timer.is_some() => {
                let now = tokio::time::Instant::now();
                match timers.iter().position(|(t, _)| *t <= now) {
                    Some(i) => match timers.remove(i).1 {
                        Timer::Settle { generation } => MonitorMsg::SettleElapsed { generation },
                        Timer::Retry => MonitorMsg::RetryTimer,
                    },
                    None => continue,
                }
            }
            Some(r) = calls.join_next(), if !calls.is_empty() => {
                // A call task that panicked never reported: treat it as a failed attempt so the
                // machine does not wait for ever.
                match (r.is_err(), writer.inflight) {
                    (true, Some(generation)) => {
                        // Nothing queued behind the dead call may be sent later: it would land
                        // after a newer set and leave Mutter with the older one.
                        writer.inflight = None;
                        writer.queued = None;
                        MonitorMsg::GrabsTimeout { generation }
                    }
                    _ => continue,
                }
            }
            _ = watchdog.tick() => {
                // A modifier "held" with no key activity at all for this long is not held: a key
                // that is down repeats about 23 times a second. The release was lost; forget it
                // and let the waiting grab change go ahead (re-sync, never a stuck deferral).
                if !held.is_empty() && last_key.elapsed() >= HELD_QUIET {
                    tracing::warn!(?held, "no key activity while a modifier is held; resynchronising");
                    held.clear();
                    if let Some(l) = &src.links {
                        l.session.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                    }
                }
                match take_deferred(&mut deferred, &held) {
                    Some(set) => MonitorMsg::SetGrabSet(set),
                    None => continue,
                }
            }
        };
        let (next, effects) = state.step(
            msg,
            &StepCtx {
                now: std::time::Instant::now(),
            },
        );
        state = next;
        pending = effects;
    }
}

async fn sleep_until(t: Option<tokio::time::Instant>) {
    if let Some(t) = t {
        tokio::time::sleep_until(t).await;
    }
}

/// The modifier grabs Mutter has or is about to have.
fn current_modifiers(state: &MonitorState) -> Option<&std::collections::HashSet<u32>> {
    match state {
        MonitorState::Watching {
            grabs: GrabSetState::Committed { set, .. },
        } => Some(&set.modifiers),
        MonitorState::Watching {
            grabs: GrabSetState::Pending { wanted, .. },
        } => Some(&wanted.1.modifiers),
        MonitorState::Unauthorized { .. }
        | MonitorState::Authorizing { .. }
        | MonitorState::Lost { .. } => None,
    }
}

fn modifiers_change(state: &MonitorState, want: &GrabSet) -> bool {
    current_modifiers(state).is_some_and(|m| *m != want.modifiers)
}

/// Remember which grabbed modifiers are down.
///
/// Every key that is, or may become, a gorca modifier counts, whether or not it is grabbed right
/// now: after pass-next-key nothing is grabbed, and the modifier still held must not have its
/// grab changed under it either (F1).
fn track_held(held: &mut Vec<u32>, state: &MonitorState, may_be: &[u32], ev: &KeyEvent) {
    let grabbed = may_be.contains(&ev.keysym)
        || current_modifiers(state).is_some_and(|m| m.contains(&ev.keysym));
    if ev.pressed {
        if grabbed && !held.contains(&ev.keysym) {
            held.push(ev.keysym);
        }
    } else {
        held.retain(|k| *k != ev.keysym);
    }
}

/// The grab set that was waiting for the modifiers to come up, once they have.
fn take_deferred(deferred: &mut Option<GrabSet>, held: &[u32]) -> Option<GrabSet> {
    if held.is_empty() {
        deferred.take()
    } else {
        None
    }
}

/// Publish the committed and wanted sets to the input lane when they change.
fn publish(
    state: &MonitorState,
    links: Option<&Arc<LaneLinks>>,
    last: &mut Option<(u32, Option<u32>)>,
) {
    let (Some(links), MonitorState::Watching { grabs }) = (links, state) else {
        return;
    };
    let key = match grabs {
        GrabSetState::Committed { generation, .. } => (*generation, None),
        GrabSetState::Pending {
            committed, wanted, ..
        } => (committed.0, Some(wanted.0)),
    };
    if *last == Some(key) {
        return;
    }
    *last = Some(key);
    let view = match grabs {
        GrabSetState::Committed { generation, set } => GrabView {
            committed_gen: *generation,
            committed: Arc::new(set.clone()),
            wanted_gen: None,
            wanted: None,
        },
        GrabSetState::Pending {
            committed, wanted, ..
        } => GrabView {
            committed_gen: committed.0,
            committed: Arc::new(committed.1.clone()),
            wanted_gen: Some(wanted.0),
            wanted: Some(Arc::new(wanted.1.clone())),
        },
    };
    links.view.store(Arc::new(view));
}

/// Serialises every grab mutation (F2). Mutter applies calls in arrival order, so the order
/// of the wire calls is the order of the machine's generations: at most one set is in flight,
/// and a newer wanted set replaces any that is still waiting.
#[derive(Debug, Default)]
struct GrabWriter {
    inflight: Option<u32>,
    queued: Option<(u32, GrabSet)>,
}

impl GrabWriter {
    /// A set to send now, if none is in flight; otherwise it waits (replacing an older waiter).
    fn submit(&mut self, generation: u32, set: GrabSet) -> Option<(u32, GrabSet)> {
        if self.inflight.is_some() {
            self.queued = Some((generation, set));
            None
        } else {
            self.inflight = Some(generation);
            Some((generation, set))
        }
    }

    /// The in-flight call finished: the next set to send, if one waited.
    fn done(&mut self) -> Option<(u32, GrabSet)> {
        self.inflight = None;
        let next = self.queued.take()?;
        self.inflight = Some(next.0);
        Some(next)
    }
}

/// `SetKeyGrabs` then `GrabKeyboard` or `UngrabKeyboard`, back to back in one task, so nothing
/// can interleave between the two calls of one set.
fn spawn_grab_call(
    calls: &mut JoinSet<()>,
    conn: &zbus::Connection,
    tx: &mpsc::Sender<MonitorMsg>,
    clock: &gorca_rt::Clock,
    generation: u32,
    set: GrabSet,
) {
    let conn = conn.clone();
    let tx = tx.clone();
    let clock = clock.clone();
    calls.spawn(async move {
        let deadline = Deadline::after(&clock, GRAB_DEADLINE);
        let wire = set.to_wire();
        let mut r = call_method(
            &conn,
            deadline,
            MANAGER_NAME,
            MANAGER_PATH,
            MONITOR_INTERFACE,
            "SetKeyGrabs",
            &wire,
        )
        .await;
        if r.is_ok() {
            // Learn mode grabs the whole keyboard (LM-1); every other set releases it.
            let deadline = Deadline::after(&clock, GRAB_DEADLINE);
            r = call_method(
                &conn,
                deadline,
                MANAGER_NAME,
                MANAGER_PATH,
                MONITOR_INTERFACE,
                if set.grab_all {
                    "GrabKeyboard"
                } else {
                    "UngrabKeyboard"
                },
                &(),
            )
            .await;
        }
        let msg = match r {
            Ok(_) => MonitorMsg::GrabsOk { generation },
            Err(e) => {
                tracing::warn!(error = %e, generation, "SetKeyGrabs failed");
                MonitorMsg::GrabsTimeout { generation }
            }
        };
        let _ = tx.send(msg).await;
    });
}

#[allow(clippy::too_many_arguments)]
fn exec(
    e: MonitorEffect,
    conn: &zbus::Connection,
    grabs: &gorca_rt::watch::Receiver<GrabSet>,
    settle: Duration,
    res_tx: &mpsc::Sender<MonitorMsg>,
    calls: &mut JoinSet<()>,
    timers: &mut Vec<(tokio::time::Instant, Timer)>,
    state: &mut MonitorState,
    pending: &mut Vec<MonitorEffect>,
    writer: &mut GrabWriter,
    ctx: &KeySourceCtx,
) {
    match e {
        MonitorEffect::SendGrabs { generation, set } => {
            if let Some((generation, set)) = writer.submit(generation, set) {
                spawn_grab_call(calls, conn, res_tx, &ctx.clock, generation, set);
            }
        }
        MonitorEffect::ArmSettle { generation, after } => {
            let _ = after;
            timers.push((
                tokio::time::Instant::now() + settle,
                Timer::Settle { generation },
            ));
        }
        MonitorEffect::ApplyModeGrabs => {
            let (next, effects) =
                std::mem::replace(state, MonitorState::Unauthorized { queued: false }).step(
                    MonitorMsg::SetGrabSet(grabs.borrow().clone()),
                    &StepCtx {
                        now: std::time::Instant::now(),
                    },
                );
            *state = next;
            pending.extend(effects);
        }
        MonitorEffect::ArmRetry { at } => {
            let _ = at;
            timers.push((tokio::time::Instant::now() + BACKOFF_MIN, Timer::Retry));
        }
        // These belong to the connection phase, which the session runs inline.
        MonitorEffect::RequestName
        | MonitorEffect::WatchKeyboard { .. }
        | MonitorEffect::Notify(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_event_body_is_decoded() {
        let ev = decode_key_event((false, 0b101, 0x61, 0x61, 38));
        assert!(ev.pressed);
        assert_eq!((ev.keycode, ev.keysym, ev.unichar), (38, 0x61, 0x61));
        assert!(ev.modifiers.contains(Modifiers::SHIFT | Modifiers::CONTROL));
        assert!(!decode_key_event((true, 0, 1, 0, 1)).pressed);
    }

    fn signal(
        path: &str,
        iface: &str,
        member: &str,
        sender: &str,
        body: &(impl zbus::export::serde::Serialize + zbus::zvariant::DynamicType),
    ) -> zbus::Message {
        zbus::Message::signal(path, iface, member)
            .and_then(|b| b.sender(sender))
            .and_then(|b| b.build(body))
            .unwrap()
    }

    fn key_signal(sender: &str) -> zbus::Message {
        signal(
            MANAGER_PATH,
            MONITOR_INTERFACE,
            "KeyEvent",
            sender,
            &(false, 0u32, 0x61u32, 0x61u32, 38u16),
        )
    }

    fn owner_signal(sender: &str, name: &str, new: &str) -> zbus::Message {
        signal(
            "/org/freedesktop/DBus",
            BUS_DAEMON,
            "NameOwnerChanged",
            sender,
            &(name, ":1.7", new),
        )
    }

    #[test]
    fn only_the_manager_owner_is_believed_and_the_rest_is_counted() {
        let mut g = Gate::new();
        // Before the owner is known nothing from the manager is believed (not even a real one).
        assert!(g.key_event(&key_signal(":1.7")).is_none());
        g.set_manager(":1.7".to_owned());
        assert_eq!(
            g.key_event(&key_signal(":1.7")).map(|e| e.keysym),
            Some(0x61)
        );
        assert!(
            g.key_event(&key_signal(":1.99")).is_none(),
            "another client"
        );
        assert!(g.key_event(&key_signal(":1.8")).is_none());
        assert_eq!(g.rejected(), 3);
        // After a restart under a new unique name, the new owner is accepted, the old one not.
        let mut g = Gate::new();
        g.set_manager(":1.42".to_owned());
        assert!(g.key_event(&key_signal(":1.42")).is_some());
        assert!(g.key_event(&key_signal(":1.7")).is_none());
    }

    #[test]
    fn the_right_sender_with_the_wrong_shape_is_still_dropped() {
        let mut g = Gate::new();
        g.set_manager(":1.7".to_owned());
        let body = (false, 0u32, 0x61u32, 0x61u32, 38u16);
        for (path, iface, member) in [
            ("/x", MONITOR_INTERFACE, "KeyEvent"),
            (MANAGER_PATH, "org.example.Other", "KeyEvent"),
            (MANAGER_PATH, MONITOR_INTERFACE, "Other"),
        ] {
            assert!(
                g.key_event(&signal(path, iface, member, ":1.7", &body))
                    .is_none()
            );
        }
        // A method call is not a signal, whoever sends it.
        let call = zbus::Message::method_call(MANAGER_PATH, "KeyEvent")
            .and_then(|b| b.interface(MONITOR_INTERFACE))
            .and_then(|b| b.sender(":1.7"))
            .and_then(|b| b.build(&body))
            .unwrap();
        assert!(g.key_event(&call).is_none());
        // A malformed body from the right sender is dropped without a panic.
        let bad = signal(MANAGER_PATH, MONITOR_INTERFACE, "KeyEvent", ":1.7", &"text");
        assert!(g.key_event(&bad).is_none());
    }

    #[test]
    fn owner_changes_count_only_from_the_bus_daemon_about_the_right_name() {
        let mut g = Gate::new();
        g.set_manager(":1.7".to_owned());
        // Forged by a client (even by the manager itself): no state change.
        assert!(
            g.owner_change(&owner_signal(":1.99", MANAGER_NAME, ""), MANAGER_NAME)
                .is_none()
        );
        assert!(
            g.owner_change(&owner_signal(":1.7", MANAGER_NAME, ""), MANAGER_NAME)
                .is_none()
        );
        // The real daemon, but about another name.
        assert!(
            g.owner_change(&owner_signal(BUS_DAEMON, "org.other", ""), MANAGER_NAME)
                .is_none()
        );
        // The real one.
        assert_eq!(
            g.owner_change(&owner_signal(BUS_DAEMON, MANAGER_NAME, ""), MANAGER_NAME),
            Some((":1.7".to_owned(), String::new()))
        );
        assert_eq!(
            g.rejected(),
            2,
            "the daemon's signal about another name is not forged, just not ours"
        );
    }

    /// zbus decides what a match rule with `sender=org.freedesktop.DBus` lets through. The gate
    /// does not rely on it, but a zbus upgrade that changed it should be noticed.
    #[test]
    fn the_owner_rule_does_not_match_a_forged_sender_in_this_zbus() {
        let r = rule(
            BUS_DAEMON,
            BUS_DAEMON,
            "NameOwnerChanged",
            Some(MANAGER_NAME),
        )
        .unwrap();
        let forged = owner_signal(":1.99", MANAGER_NAME, "");
        let genuine = owner_signal(BUS_DAEMON, MANAGER_NAME, "");
        // Pinned today: the forged one is filtered by zbus itself. If this ever flips the gate
        // above still drops it, and this test says where to look.
        assert!(
            !r.matches(&forged).unwrap_or(false),
            "zbus now lets a forged owner change through the rule"
        );
        let _ = genuine;
    }

    #[test]
    fn a_dead_grab_task_cannot_leave_an_older_set_queued() {
        let set = |n: u32| {
            let mut g = GrabSet::default();
            g.modifiers.insert(n);
            g
        };
        let mut w = GrabWriter::default();
        w.submit(1, set(1));
        w.submit(2, set(2));
        // The task for generation 1 panicked: the loop clears both fields.
        w.inflight = None;
        w.queued = None;
        assert_eq!(w.submit(3, set(3)).map(|(g, _)| g), Some(3));
        assert!(
            w.done().is_none(),
            "no stale generation 2 comes out after 3"
        );
    }

    #[test]
    fn modifier_grabs_wait_for_the_modifier_to_come_up() {
        let mut with_ins = GrabSet::default();
        with_ins.modifiers.insert(0xff63);
        let state = MonitorState::Watching {
            grabs: GrabSetState::Committed {
                generation: 1,
                set: with_ins,
            },
        };
        let ev = |sym, pressed| KeyEvent {
            keycode: 1,
            keysym: sym,
            unichar: 0,
            modifiers: Modifiers::empty(),
            pressed,
        };
        let mut held = Vec::new();
        track_held(&mut held, &state, &[], &ev(0xff63, true));
        track_held(&mut held, &state, &[], &ev(0x74, true));
        assert_eq!(held, [0xff63], "only grabbed modifiers are tracked");
        let none = GrabSet::default();
        assert!(
            modifiers_change(&state, &none),
            "pass-next-key empties the modifiers"
        );
        assert!(
            !modifiers_change(
                &state,
                &GrabSet {
                    grab_all: true,
                    modifiers: [0xff63].into_iter().collect(),
                    ..GrabSet::default()
                }
            ),
            "learn mode keeps them"
        );
        let mut deferred = Some(none);
        assert!(take_deferred(&mut deferred, &held).is_none());
        track_held(&mut held, &state, &[], &ev(0xff63, false));
        assert!(take_deferred(&mut deferred, &held).is_some());
        assert!(deferred.is_none());
    }

    #[test]
    fn a_modifier_held_after_pass_next_key_is_still_tracked_and_defers_the_restore() {
        // Pass-next-key emptied the grab set; Insert goes down and is not grabbed any more.
        let state = MonitorState::Watching {
            grabs: GrabSetState::Committed {
                generation: 2,
                set: GrabSet::default(),
            },
        };
        let ev = |pressed| KeyEvent {
            keycode: 118,
            keysym: 0xff63,
            unichar: 0,
            modifiers: Modifiers::empty(),
            pressed,
        };
        let mut held = Vec::new();
        track_held(&mut held, &state, &[0xff63], &ev(true));
        assert_eq!(held, [0xff63], "tracked although nothing is grabbed (F1)");
        let mut restore = GrabSet::default();
        restore.modifiers.insert(0xff63);
        let mut deferred = Some(restore);
        assert!(
            take_deferred(&mut deferred, &held).is_none(),
            "not while Insert is down"
        );
        track_held(&mut held, &state, &[0xff63], &ev(false));
        assert!(take_deferred(&mut deferred, &held).is_some());
    }

    #[test]
    fn grab_calls_are_serialised_and_only_the_newest_waiter_survives() {
        let set = |n: u32| {
            let mut g = GrabSet::default();
            g.modifiers.insert(n);
            g
        };
        let mut w = GrabWriter::default();
        assert_eq!(w.submit(1, set(1)).map(|(g, _)| g), Some(1));
        // While generation 1 is in flight, 2 and 3 wait; only 3 survives.
        assert!(w.submit(2, set(2)).is_none());
        assert!(w.submit(3, set(3)).is_none());
        assert_eq!(w.inflight, Some(1));
        let (g, s) = w.done().expect("the newest waiter goes next");
        assert_eq!((g, s), (3, set(3)));
        assert_eq!(w.inflight, Some(3));
        assert!(w.done().is_none(), "nothing left, the writer is idle");
        assert!(w.inflight.is_none());
        assert_eq!(w.submit(4, set(4)).map(|(g, _)| g), Some(4));
    }

    #[test]
    fn name_replies_map_by_number() {
        assert_eq!(NameReply::from(1), NameReply::Primary);
        assert_eq!(NameReply::from(3), NameReply::Exists);
        assert_eq!(NameReply::from(9), NameReply::Other(9));
    }

    #[test]
    fn the_grab_view_follows_the_machine() {
        let links = LaneLinks::new();
        let mut set = GrabSet::default();
        set.modifiers.insert(0xff63);
        let mut last = None;
        let committed = MonitorState::Watching {
            grabs: GrabSetState::Committed {
                generation: 1,
                set: GrabSet::default(),
            },
        };
        publish(&committed, Some(&links), &mut last);
        assert_eq!(links.view.load().committed_gen, 1);
        assert!(links.view.load().wanted.is_none());
        let pending = MonitorState::Watching {
            grabs: GrabSetState::Pending {
                committed: (1, GrabSet::default()),
                wanted: (2, set.clone()),
                sent_at: std::time::Instant::now(),
                retries: 0,
            },
        };
        publish(&pending, Some(&links), &mut last);
        let v = links.view.load();
        assert_eq!((v.committed_gen, v.wanted_gen), (1, Some(2)));
        assert_eq!(v.wanted.as_deref(), Some(&set));
        // Unchanged generations publish nothing new.
        let before = links.view.load_full();
        publish(&pending, Some(&links), &mut last);
        assert!(Arc::ptr_eq(&before, &links.view.load_full()));
        // Not watching: the view is left alone.
        publish(
            &MonitorState::Unauthorized { queued: false },
            Some(&links),
            &mut last,
        );
        assert!(Arc::ptr_eq(&before, &links.view.load_full()));
    }
}
