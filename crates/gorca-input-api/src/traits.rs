use gorca_rt::{BoxFuture, CancellationToken, Clock, Deadline, mpsc, watch};

use crate::event::KeyEvent;
use crate::grab::GrabSet;

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum KeySourceKind {
    /// `org.freedesktop.a11y.KeyboardMonitor` (Mutter).
    Mutter,
    /// libei / evdev fallback.
    Libei,
    /// Newton `KeyEvent` source, deferred (R5).
    Newton,
    Fake,
}

#[derive(Debug, thiserror::Error)]
pub enum KeyError {
    #[error("key source failed to start: {0}")]
    Startup(String),
    #[error("key source transport failure: {0}")]
    Transport(String),
}

/// Non-blocking producer side of the key channel. A full channel is a bug on the input lane
/// (section 6.2: "overflow is a bug; logged"), so `push` reports it instead of waiting.
#[derive(Clone, Debug)]
pub struct KeySink {
    tx: mpsc::Sender<KeyEvent>,
}

impl KeySink {
    /// `false` when the event was dropped.
    pub fn push(&self, ev: KeyEvent) -> bool {
        self.tx.try_send(ev).is_ok()
    }
}

/// Bound of the input to Reader priority lane (section 6.2: 64).
pub const KEY_QUEUE: usize = 64;

pub fn key_channel() -> (KeySink, mpsc::Receiver<KeyEvent>) {
    let (tx, rx) = mpsc::channel(KEY_QUEUE);
    (KeySink { tx }, rx)
}

pub struct KeySourceCtx {
    pub sink: KeySink,
    /// The grab set the source should have applied. Only the latest value matters ("the
    /// latest wins", section 7.3), hence a watch channel.
    pub grabs: watch::Receiver<GrabSet>,
    pub shutdown: CancellationToken,
    pub clock: Clock,
}

impl std::fmt::Debug for KeySourceCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeySourceCtx").finish_non_exhaustive()
    }
}

pub trait KeySource: Send + 'static {
    fn kind(&self) -> KeySourceKind;
    /// Runs until shutdown. Every IPC call inside has a deadline (K-4).
    fn run(self: Box<Self>, ctx: KeySourceCtx) -> BoxFuture<'static, Result<(), KeyError>>;
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct InjectKey {
    pub keycode: u16,
    pub pressed: bool,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum InjectError {
    /// No RemoteDesktop/EIS session (the portal needs user consent).
    #[error("key injection is not available")]
    NotAvailable,
    #[error("injection timed out")]
    Timeout,
    #[error("injection failed")]
    Failed,
}

/// Re-injection of swallowed keys (K-2): a fallback, not the normal path.
pub trait KeyInjector: Send + Sync + 'static {
    fn inject(&self, key: InjectKey, deadline: Deadline) -> BoxFuture<'_, Result<(), InjectError>>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Modifiers;

    #[test]
    fn key_sink_reports_overflow_instead_of_blocking() {
        let (sink, mut rx) = key_channel();
        let ev = KeyEvent {
            keycode: 1,
            keysym: 1,
            unichar: 0,
            modifiers: Modifiers::empty(),
            pressed: true,
        };
        for _ in 0..KEY_QUEUE {
            assert!(sink.push(ev));
        }
        assert!(!sink.push(ev));
        assert!(rx.try_recv().is_ok());
        assert!(sink.push(ev));
    }
}
