//! `KeySource` for Mutter's `org.freedesktop.a11y.KeyboardMonitor` (ADR-0001 sections 5.5, 7.3).
//!
//! Slice scope: own `org.gnome.Orca.KeyboardMonitor` (ID-1) **without ever taking it from a
//! running Orca**, subscribe to the unicast `KeyEvent` signal, call `WatchKeyboard`, and push
//! every event to the [`KeySink`]. Grab sets (`SetKeyGrabs`) and the full `MonitorState` machine
//! are not driven yet: gorca only observes keys, so Mutter keeps handling them normally.
//!
//! Every D-Bus call has a deadline (K-4). A missing manager, a refused name or a lost stream
//! retries with backoff; the source never gives up while the daemon runs.

use std::time::Duration;

use futures_lite::StreamExt;
use gorca_dbus::{DbusError, MAX_QUEUED, call_method, with_deadline};
use gorca_input_api::{KeyError, KeyEvent, KeySource, KeySourceCtx, KeySourceKind, Modifiers};
use gorca_rt::{BoxFuture, BudgetClass, Deadline};

use crate::{MANAGER_NAME, MANAGER_PATH, MONITOR_INTERFACE, ORCA_MONITOR_NAME};

/// `DBUS_NAME_FLAG_DO_NOT_QUEUE`: never wait in line behind a running Orca.
const DO_NOT_QUEUE: u32 = 4;
const BACKOFF_MIN: Duration = Duration::from_millis(100);
const BACKOFF_MAX: Duration = Duration::from_secs(5);
/// While another screen reader holds the name, look again this often.
const HELD_RETRY: Duration = Duration::from_secs(5);

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
    /// Session bus address; `None` means `DBUS_SESSION_BUS_ADDRESS`.
    pub session_address: Option<String>,
}

impl MutterKeySource {
    pub fn new(session_address: Option<String>) -> Self {
        Self { session_address }
    }
}

impl KeySource for MutterKeySource {
    fn kind(&self) -> KeySourceKind {
        KeySourceKind::Mutter
    }

    fn run(self: Box<Self>, ctx: KeySourceCtx) -> BoxFuture<'static, Result<(), KeyError>> {
        Box::pin(async move {
            let mut backoff = BACKOFF_MIN;
            loop {
                let started = tokio::time::Instant::now();
                let err = tokio::select! {
                    () = ctx.shutdown.cancelled() => return Ok(()),
                    r = session(&self, &ctx) => r,
                };
                let wait = match err {
                    Err(Attempt::Held) => {
                        tracing::info!(
                            "another screen reader holds the keyboard monitor; waiting for it"
                        );
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

async fn session(src: &MutterKeySource, ctx: &KeySourceCtx) -> Result<(), Attempt> {
    let deadline = Deadline::for_class(&ctx.clock, BudgetClass::Startup);
    let builder = match &src.session_address {
        Some(a) => zbus::connection::Builder::address(a.as_str()),
        None => zbus::connection::Builder::session(),
    }
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

    // Subscribe before asking for events: KeyEvent is unicast with no acknowledgement (F-2).
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface(MONITOR_INTERFACE)
        .and_then(|b| b.member("KeyEvent"))
        .map(zbus::match_rule::Builder::build)
        .map_err(DbusError::Zbus)?;
    let deadline = Deadline::for_class(&ctx.clock, BudgetClass::Startup);
    let mut stream = with_deadline(
        deadline,
        zbus::MessageStream::for_match_rule(rule, &conn, Some(MAX_QUEUED)),
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

    loop {
        let Some(msg) = stream.next().await else {
            return Err(Attempt::StreamEnded);
        };
        let Ok(msg) = msg else { continue };
        let Ok(body) = msg.body().deserialize::<(bool, u32, u32, u32, u16)>() else {
            continue;
        };
        if !ctx.sink.push(decode_key_event(body)) {
            tracing::warn!("key channel full; dropped a key event");
        }
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
    fn name_replies_map_by_number() {
        assert_eq!(NameReply::from(1), NameReply::Primary);
        assert_eq!(NameReply::from(3), NameReply::Exists);
        assert_eq!(NameReply::from(9), NameReply::Other(9));
    }
}
