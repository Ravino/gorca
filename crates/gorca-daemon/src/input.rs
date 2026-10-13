//! The input pump: the daemon's glue between the key source and the Reader (ADR-0005 4).
//!
//! It owns the [`InputLane`] classifier. For every key the source delivers it classifies, applies
//! the K-3 epoch rule (inside the lane), wakes the speech lane when the epoch moved so stale
//! speech stops at once, and hands the Reader a `KeyIntent` carrying that epoch (E-4). A
//! one-second tick runs the lane's safety nets for learn mode (LM-3).

use std::sync::Arc;
use std::time::{Duration, Instant};

use gorca_core::{ControlMsg, InputNotice, ModeMsg, ReaderSenders};
use gorca_input_api::{GrabSet, InputContext, InputLane, KeyEvent, LaneAlert, LaneLinks, ModeKind};
use gorca_input_mutter::Notice;
use gorca_rt::{CancellationToken, EpochCell, mpsc, watch};
use gorca_speech::SpeechHandle;

const TICK: Duration = Duration::from_secs(1);

/// Run the pump until the key channel closes or `shutdown`.
pub async fn pump_keys(
    mut keys: mpsc::Receiver<KeyEvent>,
    senders: ReaderSenders,
    links: Arc<LaneLinks>,
    epoch: Arc<EpochCell>,
    speech: SpeechHandle,
    grabs: Arc<watch::Sender<GrabSet>>,
    shutdown: CancellationToken,
) {
    let mut lane = InputLane::new(Arc::clone(&links), epoch, Instant::now());
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            biased;
            () = shutdown.cancelled() => return,
            ev = keys.recv() => {
                let Some(ev) = ev else { return };
                let out = lane.on_key(ev, Instant::now());
                if out.epoch_advanced {
                    speech.notify_epoch();
                }
                if senders.key.try_send(out.intent).is_err() {
                    tracing::warn!("reader key lane full; dropped a key");
                }
            }
            _ = tick.tick() => {
                if let Some(alert) = lane.tick(Instant::now()) {
                    handle_alert(alert, &links, &senders, &grabs);
                }
            }
        }
    }
}

/// LM-3: the lane gives the keyboard back by itself; the Reader catches up when it can.
fn handle_alert(
    alert: LaneAlert,
    links: &LaneLinks,
    senders: &ReaderSenders,
    grabs: &watch::Sender<GrabSet>,
) {
    tracing::warn!(?alert, "leaving learn mode from the input lane");
    if alert == LaneAlert::HelpStall {
        // The Reader is not answering: hand the keyboard back without it.
        let mut safe = GrabSet {
            ctx: InputContext::of(ModeKind::Focus),
            ..GrabSet::default()
        };
        safe.modifiers
            .extend(links.keymap.load().modifiers.iter().copied());
        grabs.send_replace(safe);
    }
    if senders
        .control
        .try_send(ControlMsg::Mode(ModeMsg::InputHelpExit))
        .is_err()
    {
        tracing::warn!("reader control lane full; learn mode exit not delivered");
    }
}

/// Forward the key source's notices to the Reader (it speaks each once).
pub async fn forward_notices(
    mut rx: mpsc::Receiver<Notice>,
    senders: ReaderSenders,
    shutdown: CancellationToken,
) {
    loop {
        let n = tokio::select! {
            () = shutdown.cancelled() => return,
            n = rx.recv() => match n { Some(n) => n, None => return },
        };
        let msg = match n {
            Notice::AnotherScreenReaderHoldsKeyboard => InputNotice::HeldByOtherScreenReader,
            Notice::KeyboardCommandsUnavailable => InputNotice::Lost,
            Notice::KeyboardMonitorUnavailable => InputNotice::Unavailable,
            Notice::KeyboardRestored => InputNotice::Restored,
        };
        if senders.control.send(ControlMsg::Input(msg)).await.is_err() {
            return;
        }
    }
}
