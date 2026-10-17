//! The system clipboard owner for browse-mode copy (NVDA `script_copyToClipboard` parity).
//!
//! GNOME 48 Mutter has no data-control protocol, so a background client cannot own the
//! Wayland selection directly. `org.gnome.Mutter.RemoteDesktop` can: a session with the
//! clipboard enabled advertises mime types (`SetSelection`), and Mutter asks for the bytes with
//! the `SelectionTransfer` signal when an application pastes. Measured on Mutter 48.7 in a
//! private shell (docs/reports/ctrl-c-passthrough.md): no portal prompt, `EnableClipboard`
//! before `Start`, the signal arrives with `(mime, serial)`, `SelectionWrite(serial)` returns
//! the pipe to write.
//!
//! The session exists only while our text is the clipboard: a newer copy replaces it, and
//! another application taking the selection (`SelectionOwnerChanged` with `session-is-owner`
//! false) ends it. Every call has a deadline, so a stuck compositor stalls only this task.
//! Copied text is never logged.

use std::collections::HashMap;
use std::time::Duration;

use gorca_dbus::{DbusError, call_method, with_deadline};
use gorca_rt::{BudgetClass, CancellationToken, Clock, Deadline, mpsc};
use tokio::io::AsyncWriteExt;
use zbus::zvariant::{OwnedFd, OwnedObjectPath, OwnedValue, Value};

const RD_NAME: &str = "org.gnome.Mutter.RemoteDesktop";
const RD_PATH: &str = "/org/gnome/Mutter/RemoteDesktop";
const RD_SESSION: &str = "org.gnome.Mutter.RemoteDesktop.Session";
/// One call to Mutter, and one transfer of the bytes to the pasting application.
const CALL_TIMEOUT: Duration = Duration::from_secs(2);

/// The mime types offered for a plain text copy.
pub const TEXT_MIMES: [&str; 3] = ["text/plain;charset=utf-8", "text/plain", "UTF8_STRING"];

/// Whether a paste request for `mime` is served with the text.
pub fn serves(mime: &str) -> bool {
    TEXT_MIMES.contains(&mime)
}

#[derive(Debug, thiserror::Error)]
enum ClipError {
    #[error(transparent)]
    Dbus(#[from] DbusError),
    #[error("unexpected reply: {0}")]
    Reply(String),
    #[error("write to the paste pipe failed: {0}")]
    Pipe(String),
}

struct Live {
    path: OwnedObjectPath,
    text: String,
    signals: zbus::MessageStream,
}

fn deadline() -> Deadline {
    Deadline::after(&Clock::System, CALL_TIMEOUT)
}

async fn session_call<B>(
    conn: &zbus::Connection,
    path: &str,
    member: &str,
    body: &B,
) -> Result<zbus::Message, ClipError>
where
    B: serde::Serialize + zbus::zvariant::DynamicType,
{
    Ok(call_method(conn, deadline(), RD_NAME, path, RD_SESSION, member, body).await?)
}

async fn start(conn: &zbus::Connection, text: String) -> Result<Live, ClipError> {
    let msg = call_method(
        conn,
        deadline(),
        RD_NAME,
        RD_PATH,
        RD_NAME,
        "CreateSession",
        &(),
    )
    .await?;
    let path: OwnedObjectPath = msg
        .body()
        .deserialize()
        .map_err(|e| ClipError::Reply(e.to_string()))?;
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface(RD_SESSION)
        .and_then(|b| b.path(path.as_str()))
        .map_err(|e| ClipError::Reply(e.to_string()))?
        .build();
    let signals = with_deadline(
        deadline(),
        zbus::MessageStream::for_match_rule(rule, conn, Some(32)),
    )
    .await?;
    let live = Live {
        path,
        text,
        signals,
    };
    // EnableClipboard with the mime types is EnableClipboard + SetSelection in one step.
    let mut opts: HashMap<&str, Value<'_>> = HashMap::new();
    opts.insert("mime-types", Value::from(TEXT_MIMES.to_vec()));
    let enabled = session_call(conn, live.path.as_str(), "EnableClipboard", &(opts,)).await;
    let started = match enabled {
        Ok(_) => session_call(conn, live.path.as_str(), "Start", &()).await,
        Err(e) => Err(e),
    };
    if let Err(e) = started {
        stop(conn, &live.path).await;
        return Err(e);
    }
    Ok(live)
}

async fn stop(conn: &zbus::Connection, path: &OwnedObjectPath) {
    if let Err(e) = session_call(conn, path.as_str(), "Stop", &()).await {
        tracing::debug!(error = %e, "clipboard session stop failed");
    }
}

/// Answer one `SelectionTransfer`: hand the pasting application the bytes, or a failure.
async fn transfer(
    conn: &zbus::Connection,
    live: &Live,
    mime: &str,
    serial: u32,
) -> Result<(), ClipError> {
    if !serves(mime) {
        session_call(
            conn,
            live.path.as_str(),
            "SelectionWriteDone",
            &(serial, false),
        )
        .await?;
        return Ok(());
    }
    let msg = session_call(conn, live.path.as_str(), "SelectionWrite", &(serial,)).await?;
    let fd: OwnedFd = msg
        .body()
        .deserialize()
        .map_err(|e| ClipError::Reply(e.to_string()))?;
    let file = tokio::fs::File::from_std(std::fs::File::from(std::os::fd::OwnedFd::from(fd)));
    let bytes = live.text.as_bytes().to_vec();
    let written = tokio::time::timeout(CALL_TIMEOUT, async move {
        let mut file = file;
        file.write_all(&bytes).await?;
        file.flush().await
    })
    .await;
    let ok = matches!(written, Ok(Ok(())));
    session_call(
        conn,
        live.path.as_str(),
        "SelectionWriteDone",
        &(serial, ok),
    )
    .await?;
    if ok {
        Ok(())
    } else {
        Err(ClipError::Pipe(match written {
            Ok(Err(e)) => e.to_string(),
            Ok(Ok(())) => String::new(),
            Err(_) => "timed out".to_owned(),
        }))
    }
}

/// What a signal on our session means.
enum Sig {
    Transfer(String, u32),
    /// Another application owns the clipboard now, or the session ended.
    Lost,
    Other,
}

fn classify(msg: &zbus::Message) -> Sig {
    let header = msg.header();
    let member = header.member().map(|m| m.as_str().to_owned());
    match member.as_deref() {
        Some("SelectionTransfer") => match msg.body().deserialize::<(String, u32)>() {
            Ok((mime, serial)) => Sig::Transfer(mime, serial),
            Err(_) => Sig::Other,
        },
        Some("SelectionOwnerChanged") => {
            let owner = msg
                .body()
                .deserialize::<HashMap<String, OwnedValue>>()
                .ok()
                .and_then(|m| {
                    m.get("session-is-owner")
                        .and_then(|v| bool::try_from(v).ok())
                });
            if owner == Some(true) {
                Sig::Other
            } else {
                Sig::Lost
            }
        }
        Some("Closed") => Sig::Lost,
        Some(_) | None => Sig::Other,
    }
}

async fn next_signal(live: &mut Option<Live>) -> Option<zbus::Message> {
    use futures_lite::StreamExt as _;
    match live {
        Some(l) => l.signals.next().await.and_then(Result::ok),
        None => std::future::pending().await,
    }
}

/// Serve copy requests until `shutdown`. A newer request replaces the older text at once.
pub async fn run(
    address: String,
    mut requests: mpsc::Receiver<String>,
    shutdown: CancellationToken,
) {
    let conn = match connect(&address).await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "clipboard: no session bus; browse-mode copy is off");
            while requests.recv().await.is_some() {}
            return;
        }
    };
    let mut live: Option<Live> = None;
    loop {
        tokio::select! {
            () = shutdown.cancelled() => break,
            req = requests.recv() => {
                let Some(mut text) = req else { break };
                // Only the newest text matters.
                while let Ok(newer) = requests.try_recv() {
                    text = newer;
                }
                if let Some(old) = live.take() {
                    stop(&conn, &old.path).await;
                }
                match start(&conn, text).await {
                    Ok(l) => {
                        tracing::debug!(session = %l.path, "clipboard: session owns the selection");
                        live = Some(l);
                    }
                    Err(e) => tracing::warn!(error = %e, "clipboard: copy failed"),
                }
            }
            msg = next_signal(&mut live) => {
                let Some(msg) = msg else {
                    live = None;
                    continue;
                };
                let sig = classify(&msg);
                tracing::debug!(
                    kind = match &sig {
                        Sig::Transfer(..) => "transfer",
                        Sig::Lost => "lost",
                        Sig::Other => "other",
                    },
                    "clipboard: session signal"
                );
                match sig {
                    Sig::Transfer(mime, serial) => {
                        if let Some(l) = live.as_ref()
                            && let Err(e) = transfer(&conn, l, &mime, serial).await
                        {
                            tracing::warn!(error = %e, "clipboard: paste transfer failed");
                        }
                    }
                    Sig::Lost => {
                        if let Some(old) = live.take() {
                            stop(&conn, &old.path).await;
                        }
                    }
                    Sig::Other => {}
                }
            }
        }
    }
    if let Some(old) = live.take() {
        stop(&conn, &old.path).await;
    }
}

async fn connect(address: &str) -> Result<zbus::Connection, DbusError> {
    let builder = zbus::connection::Builder::address(address).map_err(DbusError::Zbus)?;
    let d = Deadline::for_class(&Clock::System, BudgetClass::Startup);
    with_deadline(d, builder.build()).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_mimes_are_served_and_others_refused() {
        assert!(serves("text/plain;charset=utf-8"));
        assert!(serves("UTF8_STRING"));
        assert!(!serves("image/png"));
        assert!(!serves("text/html"));
    }
}
