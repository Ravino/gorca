// SPDX-License-Identifier: GPL-3.0-or-later
//! The input pump: the daemon's glue between the key source and the Reader (ADR-0005 4).
//!
//! It owns the [`InputLane`] classifier. For every key the source delivers it classifies, applies
//! the K-3 epoch rule (inside the lane), wakes the speech lane when the epoch moved so stale
//! speech stops at once, and hands the Reader a `KeyIntent` carrying that epoch (E-4). A
//! one-second tick runs the lane's safety nets for learn mode (LM-3).

use std::sync::Arc;
use std::time::{Duration, Instant};

use gorca_core::{ControlMsg, InputNotice, ModeMsg, ReaderSenders};
use gorca_input_api::{GrabSet, InputContext, InputLane, KeyItem, LaneAlert, LaneLinks, ModeKind};
use gorca_input_mutter::Notice;
use gorca_rt::{CancellationToken, EpochCell, mpsc, watch};
use gorca_speech::SpeechHandle;

const TICK: Duration = Duration::from_secs(1);
/// Log the lane statistics every this many ticks (when they changed).
const STATS_EVERY: u64 = 60;

/// Run the pump until the key channel closes or `shutdown`.
#[allow(clippy::too_many_arguments)] // wiring: one handle per lane the pump feeds
pub async fn pump_keys(
    mut keys: mpsc::Receiver<KeyItem>,
    senders: ReaderSenders,
    links: Arc<LaneLinks>,
    epoch: Arc<EpochCell>,
    speech: SpeechHandle,
    grabs: Arc<watch::Sender<GrabSet>>,
    prewake: Arc<crate::plugins::Prewake>,
    shutdown: CancellationToken,
) {
    let mut lane = InputLane::new(Arc::clone(&links), epoch, Instant::now());
    let (mut ticks, mut last_stats) = (0u64, lane.stats());
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            biased;
            () = shutdown.cancelled() => return,
            ev = keys.recv() => {
                let Some(item) = ev else { return };
                if item.resync {
                    // Events were lost just before this one: forget at the gap, then go on.
                    lane.resync();
                }
                // The plugin workers start running now, ahead of the focus change this key may
                // cause (a rate-limited, non-blocking atomic store and a futex wake).
                prewake.key_down();
                let out = lane.on_key(item.event, Instant::now());
                if out.epoch_advanced {
                    speech.notify_epoch();
                }
                if let Some(keysym) = out.stale_modifier {
                    // Mutter 48.7 swallows every key until the modifier is tapped. No heal
                    // without input injection exists (docs/reports/live-keys-swallowed.md).
                    tracing::warn!(keysym, "grabbed modifier predicted stuck in Mutter; keys are swallowed");
                    if senders
                        .control
                        .try_send(ControlMsg::Input(InputNotice::ModifierStuck(keysym)))
                        .is_err()
                    {
                        tracing::warn!("reader control lane full; stuck-modifier hint not delivered");
                    }
                }
                if senders.key.try_send(out.intent).is_err() {
                    tracing::warn!("reader key lane full; dropped a key");
                }
            }
            _ = tick.tick() => {
                ticks += 1;
                if ticks % STATS_EVERY == 0 && lane.stats() != last_stats {
                    // So that Ambiguous keys (grab changes in flight, the double-tap guard band)
                    // are visible in the log, not only counted (M-16).
                    last_stats = lane.stats();
                    tracing::info!(
                        keys = last_stats.keys,
                        ambiguous = last_stats.ambiguous,
                        band_forks = last_stats.band_forks,
                        "input lane statistics"
                    );
                }
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
