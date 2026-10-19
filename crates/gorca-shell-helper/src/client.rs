// SPDX-License-Identifier: GPL-3.0-or-later
//! The pointer actor and its handle (ADR-0025 sections 2.5, 2.7, 2.9).
//!
//! One task owns the D-Bus calls, the [`AvailMachine`] and the request queue; the handle only
//! enqueues and awaits a reply with a deadline, so nothing here ever runs on the key to speech
//! path (EX-1, MI-2).
//!
//! - **Queue (EX-3):** one request in flight plus at most [`PENDING`] waiting (FIFO, never
//!   coalesced); overflow fails `Busy`. When the in-flight call passes its deadline every waiting
//!   request is dropped with `NotResponding` and new requests are refused until the overdue call
//!   answers late or a probe succeeds.
//! - **Deadlines (EX-2):** every call carries `not-after = now + remaining(deadline)`, so a call the
//!   Shell runs late injects nothing; the late reply is still read.
//! - **Availability:** a `watch` channel carries [`PointerAvail`] for the Reader (runtime gate).

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use futures_lite::StreamExt;
use gorca_dbus::{DbusError, call_method};
use gorca_output_api::{
    Button, ButtonAct, ClickCount, ClickOpts, OutcomeKind, PointTarget, PointerAvail, PointerError,
    PointerOutcome, PointerPos, PointerSink,
};
use gorca_rt::{BoxFuture, CancellationToken, Deadline, oneshot, watch};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use zbus::fdo::DBusProxy;
use zbus::names::BusName;
use zbus::zvariant::OwnedValue;

use crate::avail::{AvailEffect, AvailEvent, AvailMachine, Owner, POLL_EVERY, ProbeResult};
use crate::wire::{
    HELPER_INTERFACE, HELPER_NAME, HELPER_PATH, SHELL_NAME, WireTarget, button_arg, decode_outcome,
    decode_pos, map_dbus_error, map_remote_error, not_after_us, opts_body,
};

/// Requests waiting behind the one in flight (EX-3).
pub const PENDING: usize = 4;
/// Deadline of one probe step (`GetNameOwner`, `Ping`, `Capabilities`).
pub const PROBE_STEP: Duration = Duration::from_millis(150);
/// A call is abandoned this long after its caller's deadline; an answer before that clears the
/// overdue state.
pub const LATE_CAP: Duration = Duration::from_secs(2);
/// Retry of the first subscription (the bus connection is fine but the proxy failed).
const SETUP_RETRY: Duration = Duration::from_secs(5);

#[derive(Clone, Debug)]
enum Op {
    Move(WireTarget, ClickOpts),
    ClickAt(WireTarget, Button, ClickOpts),
    Button(Button, ButtonAct),
    ReleaseAll,
    GetPointer,
}

#[derive(Clone, Debug)]
enum Reply {
    Outcome(PointerOutcome),
    Released(u32),
    Pos(PointerPos),
}

struct Cmd {
    op: Op,
    deadline: Deadline,
    reply: oneshot::Sender<Result<Reply, PointerError>>,
}

/// Handle to the pointer actor. Cheap to clone the pieces through [`Self::availability`]; the
/// handle itself is shared behind an `Arc` by its users.
#[derive(Debug)]
pub struct ShellHelperClient {
    tx: mpsc::Sender<Cmd>,
    avail: watch::Receiver<PointerAvail>,
    overdue: Arc<AtomicBool>,
    cancel: CancellationToken,
}

impl ShellHelperClient {
    /// Start the actor on `conn` (the caller's session-bus connection; give it a
    /// `method_timeout` above [`LATE_CAP`]). Returns the handle and the actor task.
    pub fn start(conn: zbus::Connection) -> (Self, JoinHandle<()>) {
        let (tx, rx) = mpsc::channel(PENDING);
        let (avail_tx, avail) = watch::channel(PointerAvail::Absent);
        let overdue = Arc::new(AtomicBool::new(false));
        let cancel = CancellationToken::new();
        let actor = Actor {
            conn,
            rx,
            avail_tx,
            overdue: Arc::clone(&overdue),
            machine: AvailMachine::new(),
            reprobe: None,
            late: None,
            verified: None,
        };
        let task = tokio::spawn(actor.run(cancel.clone()));
        (
            Self {
                tx,
                avail,
                overdue,
                cancel,
            },
            task,
        )
    }

    /// Stop the actor (held buttons are released by the helper when the connection goes away).
    pub fn shutdown(&self) {
        self.cancel.cancel();
    }

    async fn submit(&self, op: Op, deadline: Deadline) -> Result<Reply, PointerError> {
        let avail = *self.avail.borrow();
        match avail {
            PointerAvail::Ready { .. } => {}
            PointerAvail::Unresponsive => return Err(PointerError::NotResponding),
            PointerAvail::Absent
            | PointerAvail::Squatted
            | PointerAvail::Denied
            | PointerAvail::TooOld { .. }
            | PointerAvail::NoCap => return Err(PointerError::Unavailable(avail)),
        }
        if self.overdue.load(Ordering::Acquire) {
            return Err(PointerError::NotResponding);
        }
        let (reply, rx) = oneshot::channel();
        match self.tx.try_send(Cmd {
            op,
            deadline,
            reply,
        }) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => return Err(PointerError::Busy),
            Err(mpsc::error::TrySendError::Closed(_)) => {
                return Err(PointerError::Unavailable(PointerAvail::Absent));
            }
        }
        match gorca_rt::with_deadline(deadline, rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => Err(PointerError::Unavailable(PointerAvail::Absent)),
            Err(_) => Err(PointerError::Timeout),
        }
    }

    /// Move the pointer to a wire target (the window matcher of MO-5a builds these).
    pub async fn move_wire(
        &self,
        target: WireTarget,
        opts: ClickOpts,
        deadline: Deadline,
    ) -> Result<PointerOutcome, PointerError> {
        outcome(self.submit(Op::Move(target, opts), deadline).await)
    }

    /// Move and click in one compositor turn.
    pub async fn click_wire(
        &self,
        target: WireTarget,
        button: Button,
        opts: ClickOpts,
        deadline: Deadline,
    ) -> Result<PointerOutcome, PointerError> {
        outcome(
            self.submit(Op::ClickAt(target, button, opts), deadline)
                .await,
        )
    }
}

fn outcome(r: Result<Reply, PointerError>) -> Result<PointerOutcome, PointerError> {
    match r? {
        Reply::Outcome(o) => Ok(o),
        Reply::Released(_) | Reply::Pos(_) => {
            Err(PointerError::Failed("unexpected reply kind".to_owned()))
        }
    }
}

impl PointerSink for ShellHelperClient {
    fn click_at(
        &self,
        target: PointTarget,
        button: Button,
        opts: ClickOpts,
        deadline: Deadline,
    ) -> BoxFuture<'_, Result<PointerOutcome, PointerError>> {
        Box::pin(async move {
            match target {
                PointTarget::Stage { x, y } => {
                    self.click_wire(WireTarget::stage(x, y), button, opts, deadline)
                        .await
                }
                // The window matcher (WM-1..4) is MO-5a; until then a window target has no location.
                PointTarget::Window { .. } => Err(PointerError::NoLocation),
            }
        })
    }

    fn move_to(
        &self,
        target: PointTarget,
        opts: ClickOpts,
        deadline: Deadline,
    ) -> BoxFuture<'_, Result<PointerOutcome, PointerError>> {
        Box::pin(async move {
            match target {
                PointTarget::Stage { x, y } => {
                    self.move_wire(WireTarget::stage(x, y), opts, deadline)
                        .await
                }
                PointTarget::Window { .. } => Err(PointerError::NoLocation),
            }
        })
    }

    fn button(
        &self,
        button: Button,
        act: ButtonAct,
        deadline: Deadline,
    ) -> BoxFuture<'_, Result<PointerOutcome, PointerError>> {
        Box::pin(async move { outcome(self.submit(Op::Button(button, act), deadline).await) })
    }

    fn release_all(&self, deadline: Deadline) -> BoxFuture<'_, Result<u32, PointerError>> {
        Box::pin(async move {
            match self.submit(Op::ReleaseAll, deadline).await? {
                Reply::Released(n) => Ok(n),
                Reply::Outcome(_) | Reply::Pos(_) => {
                    Err(PointerError::Failed("unexpected reply kind".to_owned()))
                }
            }
        })
    }

    fn position(&self, deadline: Deadline) -> BoxFuture<'_, Result<PointerPos, PointerError>> {
        Box::pin(async move {
            match self.submit(Op::GetPointer, deadline).await? {
                Reply::Pos(p) => Ok(p),
                Reply::Outcome(_) | Reply::Released(_) => {
                    Err(PointerError::Failed("unexpected reply kind".to_owned()))
                }
            }
        })
    }

    fn availability(&self) -> watch::Receiver<PointerAvail> {
        self.avail.clone()
    }
}

type Flight = JoinHandle<Result<Reply, PointerError>>;

struct Actor {
    conn: zbus::Connection,
    rx: mpsc::Receiver<Cmd>,
    avail_tx: watch::Sender<PointerAvail>,
    overdue: Arc<AtomicBool>,
    machine: AvailMachine,
    reprobe: Option<tokio::time::Instant>,
    /// The call whose deadline passed; its answer (or its hard cap) ends the overdue state.
    late: Option<Flight>,
    /// The VERIFIED unique name (`:1.x`) of the helper: owner of `org.gorca.ShellHelper` equal to the owner of
    /// `org.gnome.Shell` at the last check (MI-7). Every call goes to this unique name, never to the well-known
    /// name, so a process that takes the name after the check cannot receive anything. Cleared on every owner
    /// change until re-verified.
    verified: Option<Owner>,
}

impl Actor {
    async fn run(mut self, cancel: CancellationToken) {
        let Some(dbus) = self.setup(&cancel).await else {
            self.publish();
            return;
        };
        let Ok(mut helper_ch) = dbus
            .receive_name_owner_changed_with_args(&[(0, HELPER_NAME)])
            .await
        else {
            tracing::warn!("pointer: cannot watch {HELPER_NAME}");
            return;
        };
        let Ok(mut shell_ch) = dbus
            .receive_name_owner_changed_with_args(&[(0, SHELL_NAME)])
            .await
        else {
            tracing::warn!("pointer: cannot watch {SHELL_NAME}");
            return;
        };
        let mut poll = tokio::time::interval(POLL_EVERY);
        poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        self.refresh_owners(&dbus).await;
        loop {
            tokio::select! {
                () = cancel.cancelled() => break,
                s = helper_ch.next() => {
                    if s.is_none() { break; }
                    self.verified = None;
                    self.refresh_owners(&dbus).await;
                }
                s = shell_ch.next() => {
                    if s.is_none() { break; }
                    self.verified = None;
                    self.refresh_owners(&dbus).await;
                }
                _ = poll.tick() => self.apply(AvailEvent::PollTick).await,
                () = sleep_until_opt(self.reprobe), if self.reprobe.is_some() => {
                    self.reprobe = None;
                    self.apply(AvailEvent::ReprobeDue).await;
                }
                done = late_done(&mut self.late), if self.late.is_some() => {
                    self.late = None;
                    self.on_late(done).await;
                }
                cmd = self.rx.recv() => match cmd {
                    Some(c) => self.exec(c).await,
                    None => break,
                },
            }
        }
        // Dropping the connection releases held buttons in the helper (H-3).
        self.verified = None;
        self.machine = AvailMachine::new();
        self.publish();
    }

    async fn setup(&mut self, cancel: &CancellationToken) -> Option<DBusProxy<'static>> {
        loop {
            match DBusProxy::new(&self.conn).await {
                Ok(p) => return Some(p),
                Err(e) => {
                    tracing::warn!("pointer: bus proxy failed: {e}");
                    tokio::select! {
                        () = cancel.cancelled() => return None,
                        () = tokio::time::sleep(SETUP_RETRY) => {}
                    }
                }
            }
        }
    }

    fn publish(&self) {
        let now = self.machine.avail();
        self.avail_tx.send_if_modified(|v| {
            if *v == now {
                false
            } else {
                *v = now;
                true
            }
        });
    }

    async fn owner_of(dbus: &DBusProxy<'static>, name: &'static str) -> Option<Owner> {
        let bus: BusName<'_> = BusName::try_from(name).ok()?;
        let d = Deadline(Instant::now() + PROBE_STEP);
        match gorca_rt::with_deadline(d, dbus.get_name_owner(bus)).await {
            Ok(Ok(o)) => Some(Arc::from(o.as_str())),
            Ok(Err(_)) | Err(_) => None,
        }
    }

    async fn refresh_owners(&mut self, dbus: &DBusProxy<'static>) {
        let helper = Self::owner_of(dbus, HELPER_NAME).await;
        let shell = Self::owner_of(dbus, SHELL_NAME).await;
        self.verified = match (&helper, &shell) {
            (Some(h), Some(s)) if h == s => Some(Arc::clone(h)),
            (Some(_) | None, Some(_) | None) => None,
        };
        self.apply(AvailEvent::Owners { helper, shell }).await;
    }

    /// Feed an event, run the effects it asks for (probes feed their own results back), publish.
    async fn apply(&mut self, ev: AvailEvent) {
        let mut queue = VecDeque::from([ev]);
        while let Some(e) = queue.pop_front() {
            for fx in self.machine.step(e) {
                match fx {
                    AvailEffect::Probe => {
                        let r = self.probe().await;
                        if matches!(r, ProbeResult::Ok { .. }) {
                            // C-1/EX-3: a successful probe ends the overdue state.
                            self.overdue.store(false, Ordering::Release);
                        }
                        queue.push_back(AvailEvent::Probed(r));
                    }
                    AvailEffect::ReprobeAfter(d) => {
                        self.reprobe = Some(tokio::time::Instant::now() + d);
                    }
                }
            }
        }
        self.publish();
    }

    /// `Ping` then `Capabilities`, 150 ms each.
    async fn probe(&self) -> ProbeResult {
        let Some(dest) = self.verified.clone() else {
            return ProbeResult::Failed;
        };
        let d = Deadline(Instant::now() + PROBE_STEP);
        let version = match call_method(
            &self.conn,
            d,
            &dest,
            HELPER_PATH,
            HELPER_INTERFACE,
            "Ping",
            &(),
        )
        .await
        {
            Ok(m) if !from(&m, &dest) => return ProbeResult::Failed,
            Ok(m) => match m.body().deserialize::<u32>() {
                Ok(v) => v,
                Err(_) => return ProbeResult::Failed,
            },
            Err(e) => return probe_failure(&e),
        };
        let d = Deadline(Instant::now() + PROBE_STEP);
        let caps = match call_method(
            &self.conn,
            d,
            &dest,
            HELPER_PATH,
            HELPER_INTERFACE,
            "Capabilities",
            &(),
        )
        .await
        {
            Ok(m) if !from(&m, &dest) => return ProbeResult::Failed,
            Ok(m) => match m.body().deserialize::<Vec<String>>() {
                Ok(v) => v,
                Err(_) => return ProbeResult::Failed,
            },
            Err(e) => return probe_failure(&e),
        };
        ProbeResult::Ok {
            version,
            pointer: caps.iter().any(|c| c == "pointer"),
        }
    }

    async fn exec(&mut self, cmd: Cmd) {
        let Cmd {
            op,
            deadline,
            reply,
        } = cmd;
        let avail = self.machine.avail();
        if self.overdue.load(Ordering::Acquire) || avail == PointerAvail::Unresponsive {
            let _ = reply.send(Err(PointerError::NotResponding));
            return;
        }
        if !avail.can_call() {
            let _ = reply.send(Err(PointerError::Unavailable(avail)));
            return;
        }
        let now = Instant::now();
        if deadline.expired(now) {
            // Waited in the queue past its deadline: nothing is sent, so nothing can be injected.
            let _ = reply.send(Err(PointerError::Timeout));
            return;
        }
        let Some(dest) = self.verified.clone() else {
            // Not (re-)verified since the last owner change: nothing is dispatched.
            let _ = reply.send(Err(PointerError::Unavailable(avail)));
            return;
        };
        let not_after = not_after_us(deadline, now);
        let conn = self.conn.clone();
        let hard = Deadline(deadline.0 + LATE_CAP);
        let mut flight: Flight = tokio::spawn(send_op(conn, dest, hard, op, not_after));
        if let Ok(joined) = gorca_rt::with_deadline(deadline, &mut flight).await {
            let res = joined
                .unwrap_or_else(|e| Err(PointerError::Failed(format!("call task failed: {e}"))));
            let ev = classify(&res);
            let _ = reply.send(res);
            self.apply(ev).await;
        } else {
            let _ = reply.send(Err(PointerError::Timeout));
            self.overdue.store(true, Ordering::Release);
            self.late = Some(flight);
            while let Ok(c) = self.rx.try_recv() {
                let _ = c.reply.send(Err(PointerError::NotResponding));
            }
            self.apply(AvailEvent::CallMissed).await;
        }
    }

    /// The overdue call finished (answer, error, or its hard cap).
    async fn on_late(&mut self, done: Result<Result<Reply, PointerError>, tokio::task::JoinError>) {
        match done {
            Ok(Ok(Reply::Outcome(o))) if o.kind == OutcomeKind::Done => {
                // Cannot happen when the helper honours `not-after`: a helper bug signal.
                tracing::warn!("pointer: a call answered `done` after its deadline");
                self.overdue.store(false, Ordering::Release);
                self.apply(AvailEvent::LateAnswer).await;
            }
            Ok(Ok(_)) => {
                self.overdue.store(false, Ordering::Release);
                self.apply(AvailEvent::LateAnswer).await;
            }
            Ok(Err(e)) => {
                let silent = matches!(
                    e,
                    PointerError::Timeout
                        | PointerError::NotResponding
                        | PointerError::NoLocation
                        | PointerError::Unavailable(_)
                );
                if silent {
                    self.probe_to_clear_overdue().await;
                } else {
                    // An error reply is an answer from a live helper.
                    self.overdue.store(false, Ordering::Release);
                    self.apply(AvailEvent::LateAnswer).await;
                }
            }
            // The call never answered within its hard cap, or its task died: only a probe clears it.
            Err(_) => self.probe_to_clear_overdue().await,
        }
    }

    async fn probe_to_clear_overdue(&mut self) {
        // ReprobeDue probes while the helper is owned by the Shell's owner.
        self.apply(AvailEvent::ReprobeDue).await;
    }
}

async fn sleep_until_opt(t: Option<tokio::time::Instant>) {
    match t {
        Some(t) => tokio::time::sleep_until(t).await,
        None => std::future::pending::<()>().await,
    }
}

async fn late_done(
    late: &mut Option<Flight>,
) -> Result<Result<Reply, PointerError>, tokio::task::JoinError> {
    match late {
        Some(j) => j.await,
        None => std::future::pending().await,
    }
}

/// The reply came from the verified unique name.
fn from(m: &zbus::Message, dest: &str) -> bool {
    m.header().sender().is_some_and(|s| s.as_str() == dest)
}

fn probe_failure(e: &DbusError) -> ProbeResult {
    match e {
        DbusError::Timeout => ProbeResult::Timeout,
        DbusError::Zbus(zbus::Error::MethodError(name, desc, _)) => {
            match map_remote_error(name.as_str(), desc.as_deref().unwrap_or("")) {
                PointerError::Denied => ProbeResult::Denied,
                PointerError::Locked
                | PointerError::Busy
                | PointerError::RateLimited
                | PointerError::NoWindow
                | PointerError::BadArgs(_)
                | PointerError::NoLocation
                | PointerError::Timeout
                | PointerError::NotResponding
                | PointerError::Unavailable(_)
                | PointerError::Failed(_) => ProbeResult::Failed,
            }
        }
        DbusError::Zbus(_) => ProbeResult::Failed,
    }
}

/// What the machine should hear about a finished call.
fn classify(res: &Result<Reply, PointerError>) -> AvailEvent {
    match res {
        Ok(Reply::Outcome(o)) if o.kind == OutcomeKind::Expired => AvailEvent::CallMissed,
        Err(PointerError::Timeout) => AvailEvent::CallMissed,
        Err(PointerError::Denied) => AvailEvent::CallDenied,
        // Locked, busy, rate limited and the like are answers from a live helper.
        Ok(Reply::Outcome(_) | Reply::Released(_) | Reply::Pos(_))
        | Err(
            PointerError::Locked
            | PointerError::Busy
            | PointerError::RateLimited
            | PointerError::NoWindow
            | PointerError::BadArgs(_)
            | PointerError::NoLocation
            | PointerError::NotResponding
            | PointerError::Unavailable(_)
            | PointerError::Failed(_),
        ) => AvailEvent::CallAnswered,
    }
}

/// Build and send one request, decode its reply. Runs in its own task so a late answer can still
/// be read after the caller's deadline.
async fn send_op(
    conn: zbus::Connection,
    dest: Owner,
    deadline: Deadline,
    op: Op,
    not_after: i64,
) -> Result<Reply, PointerError> {
    let msg = match op {
        Op::Move(t, opts) => {
            let body = (
                t.window,
                t.origin.wire(),
                t.x,
                t.y,
                opts_body(not_after, opts),
            );
            call_method(
                &conn,
                deadline,
                &dest,
                HELPER_PATH,
                HELPER_INTERFACE,
                "PointerMove",
                &body,
            )
            .await
        }
        Op::ClickAt(t, b, opts) => {
            let body = (
                t.window,
                t.origin.wire(),
                t.x,
                t.y,
                button_arg(b),
                opts_body(not_after, opts),
            );
            call_method(
                &conn,
                deadline,
                &dest,
                HELPER_PATH,
                HELPER_INTERFACE,
                "PointerClickAt",
                &body,
            )
            .await
        }
        Op::Button(b, act) => {
            let (action, count) = match act {
                ButtonAct::Click => ("click", ClickCount::One),
                ButtonAct::DoubleClick => ("click", ClickCount::Two),
                ButtonAct::Press => ("press", ClickCount::One),
                ButtonAct::Release => ("release", ClickCount::One),
            };
            let opts = ClickOpts {
                count,
                ..ClickOpts::default()
            };
            let body = (button_arg(b), action, opts_body(not_after, opts));
            call_method(
                &conn,
                deadline,
                &dest,
                HELPER_PATH,
                HELPER_INTERFACE,
                "PointerButton",
                &body,
            )
            .await
        }
        Op::ReleaseAll => {
            call_method(
                &conn,
                deadline,
                &dest,
                HELPER_PATH,
                HELPER_INTERFACE,
                "PointerRelease",
                &(),
            )
            .await
        }
        Op::GetPointer => {
            call_method(
                &conn,
                deadline,
                &dest,
                HELPER_PATH,
                HELPER_INTERFACE,
                "GetPointer",
                &(),
            )
            .await
        }
    };
    let msg = msg.map_err(|e| map_dbus_error(&e))?;
    if !from(&msg, &dest) {
        return Err(PointerError::Failed(
            "reply from an unexpected sender".to_owned(),
        ));
    }
    let body = msg.body();
    match op {
        Op::Move(..) | Op::ClickAt(..) | Op::Button(..) => {
            let m: HashMap<String, OwnedValue> = body
                .deserialize()
                .map_err(|e| PointerError::Failed(format!("bad reply: {e}")))?;
            decode_outcome(&m).map(Reply::Outcome)
        }
        Op::ReleaseAll => body
            .deserialize::<u32>()
            .map(Reply::Released)
            .map_err(|e| PointerError::Failed(format!("bad reply: {e}"))),
        Op::GetPointer => {
            let m: HashMap<String, OwnedValue> = body
                .deserialize()
                .map_err(|e| PointerError::Failed(format!("bad reply: {e}")))?;
            decode_pos(&m).map(Reply::Pos)
        }
    }
}
