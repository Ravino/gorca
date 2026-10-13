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

/// Sender validation. `KeyEvent` is a unicast signal, so any client on the session bus can
/// address one to us; only the current owner of the Mutter a11y manager name may be believed.
#[derive(Debug)]
pub(crate) struct SenderFilter {
    owner: String,
    rejected: u64,
}

impl SenderFilter {
    pub(crate) fn new(owner: String) -> Self {
        Self { owner, rejected: 0 }
    }

    /// `true` if the header sender is the expected unique name; otherwise counts a drop.
    pub(crate) fn accept(&mut self, sender: Option<&str>) -> bool {
        if sender == Some(self.owner.as_str()) {
            true
        } else {
            self.rejected += 1;
            false
        }
    }
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

    // Who is the manager right now? Everything else claiming to be it is dropped.
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
    let mut filter = SenderFilter::new(manager_owner);
    let mut bus_filter = SenderFilter::new(BUS_DAEMON.to_owned());
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
    tracing::info!("keyboard monitor active");
    *watching = true;
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
                ctx,
            );
        }
        publish(&state, src.links.as_ref(), &mut published);
        if matches!(state, MonitorState::Lost { .. }) {
            return Err(Attempt::GrabsFailed);
        }
        let next_timer = timers.iter().map(|(t, _)| *t).min();
        let msg: MonitorMsg = tokio::select! {
            m = keys.next() => {
                let Some(m) = m else { return Err(Attempt::StreamEnded) };
                if let Ok(m) = m
                    && filter.accept(m.header().sender().map(zbus::names::UniqueName::as_str))
                    && let Ok(body) = m.body().deserialize::<(bool, u32, u32, u32, u16)>()
                    && !ctx.sink.push(decode_key_event(body))
                {
                    tracing::warn!("key channel full; dropped a key event");
                }
                continue;
            }
            m = owners.next() => {
                // Any change of the manager's owner invalidates our grabs: start over.
                if let Some(Ok(m)) = m
                    && !bus_filter.accept(m.header().sender().map(zbus::names::UniqueName::as_str))
                {
                    tracing::warn!("ignored a forged NameOwnerChanged");
                    continue;
                }
                return Err(Attempt::ManagerGone);
            }
            m = own_name.next() => {
                if let Some(Ok(m)) = m
                    && bus_filter.accept(m.header().sender().map(zbus::names::UniqueName::as_str))
                    && let Ok((_, _, new)) = m.body().deserialize::<(String, String, String)>()
                    && new.is_empty()
                {
                    return Err(Attempt::NameLost);
                }
                // The owner changed to someone else, or to us (our own acquisition echo).
                continue;
            }
            r = grabs.changed() => {
                if r.is_err() {
                    // The Reader side is gone: the daemon is shutting down.
                    return Ok(());
                }
                MonitorMsg::SetGrabSet(grabs.borrow_and_update().clone())
            }
            Some(m) = results.recv() => m,
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
            Some(_) = calls.join_next(), if !calls.is_empty() => continue,
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
    ctx: &KeySourceCtx,
) {
    match e {
        MonitorEffect::SendGrabs { generation, set } => {
            let conn = conn.clone();
            let tx = res_tx.clone();
            let clock = ctx.clock.clone();
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
                    // Both calls are idempotent in Mutter.
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

    #[test]
    fn only_the_manager_owner_is_believed_and_the_rest_is_counted() {
        let mut f = SenderFilter::new(":1.7".to_owned());
        assert!(f.accept(Some(":1.7")));
        assert!(!f.accept(Some(":1.99")), "another client on the bus");
        assert!(!f.accept(None), "no sender header");
        assert!(
            !f.accept(Some("org.freedesktop.a11y.Manager")),
            "well-known names are not senders"
        );
        assert_eq!(f.rejected, 3);
        // After a restart under a new unique name, the new owner is accepted, the old one is not.
        let mut f = SenderFilter::new(":1.42".to_owned());
        assert!(f.accept(Some(":1.42")) && !f.accept(Some(":1.7")));
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
