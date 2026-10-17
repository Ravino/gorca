// SPDX-License-Identifier: GPL-3.0-or-later
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
    tx: mpsc::Sender<KeyItem>,
    /// An event was dropped and the next one to get through must say so.
    owed: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// One entry of the key channel: the event, and whether the entries before it are incomplete.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct KeyItem {
    pub event: KeyEvent,
    /// One or more events were dropped just before this one (the channel was full): the lane
    /// must forget what it knows of the keyboard **before** handling it, so that the reset
    /// happens at the gap and not at the head of the queue.
    pub resync: bool,
}

impl KeySink {
    /// `false` when the event was dropped; the next event that gets through carries the
    /// resync mark (in band, so the lane resets exactly at the gap).
    pub fn push(&self, ev: KeyEvent) -> bool {
        use std::sync::atomic::Ordering;
        let resync = self.owed.load(Ordering::Acquire);
        if self.tx.try_send(KeyItem { event: ev, resync }).is_ok() {
            if resync {
                self.owed.store(false, Ordering::Release);
            }
            true
        } else {
            self.owed.store(true, Ordering::Release);
            false
        }
    }
}

/// Bound of the key channel from the source to the input lane. Large on purpose (speed beats
/// frugality): a held key repeats about 23 times a second, and a key event must never be lost to
/// a pump that is briefly descheduled. If it ever fills, the source resynchronises the lane.
pub const KEY_QUEUE: usize = 4096;

pub fn key_channel() -> (KeySink, mpsc::Receiver<KeyItem>) {
    let (tx, rx) = mpsc::channel(KEY_QUEUE);
    (
        KeySink {
            tx,
            owed: std::sync::Arc::default(),
        },
        rx,
    )
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

    #[test]
    fn the_event_after_a_gap_carries_the_resync_mark_exactly_once() {
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
        assert!(!sink.push(ev), "dropped: the gap");
        assert!(!sink.push(ev), "still full: the same gap");
        // Make room for exactly one more.
        let first = rx.try_recv().unwrap();
        assert!(!first.resync, "everything before the gap is plain");
        assert!(sink.push(ev));
        // Drain the queue: only the entry right after the gap is marked.
        let marked: Vec<bool> = std::iter::from_fn(|| rx.try_recv().ok())
            .map(|i| i.resync)
            .collect();
        assert_eq!(marked.iter().filter(|m| **m).count(), 1);
        assert_eq!(
            marked.last(),
            Some(&true),
            "the marked entry is the last one pushed"
        );
        assert!(sink.push(ev));
        assert!(!rx.try_recv().unwrap().resync, "and the mark is spent");
    }
}
