//! Subsystem wiring: one additive `wire_*` function per subsystem (phase-2 plan CF-2).
//!
//! Each function spawns its tasks on the shared [`TaskSet`] and returns what later wiring steps
//! need. `run_with` only calls them in order, so a milestone adds a function and one call site
//! without editing the others.

use std::sync::Arc;

use gorca_connector_api::AppRegistrar;
use gorca_connector_atspi::{AtspiOptions, AtspiState};
use gorca_input_api::{GrabSet, KeyIntent, KeySource, KeySourceCtx, key_channel};
use gorca_input_mutter::MutterKeySource;
use gorca_rt::{CancellationToken, Clock, EpochCell, FocusedApp, TaskSet, mpsc};
use gorca_speech::{FailoverClient, FailoverConfig, LaneConfig, SpeechHandle, SpeechLane};

use crate::{Options, announce_notices, pump_keys, speech_plan, supervise_connector};

/// Speech: the failover client under the speech lane, plus the failover-notice announcer.
pub(crate) fn wire_speech(
    tasks: &mut TaskSet,
    opts: &Options,
    epoch: &Arc<EpochCell>,
) -> SpeechHandle {
    let (backends, mode) = speech_plan(opts);
    let (client, notices) = FailoverClient::spawn(
        tasks,
        backends,
        FailoverConfig {
            mode,
            ..FailoverConfig::default()
        },
    );
    let speech = SpeechLane::spawn(
        tasks,
        Arc::clone(epoch),
        Box::new(client),
        LaneConfig::default(),
    );
    tasks.spawn(
        "speech-notices",
        announce_notices(notices, speech.clone(), Arc::clone(epoch)),
    );
    speech
}

/// Keyboard monitoring: the Mutter key source and the pump into the Reader's key lane. Stays
/// off unless the daemon impersonates Orca (ID-1) and knows the session bus.
pub(crate) fn wire_keyboard(
    tasks: &mut TaskSet,
    opts: &Options,
    shutdown: &CancellationToken,
    epoch: &Arc<EpochCell>,
    speech: SpeechHandle,
    reader_keys: mpsc::Sender<KeyIntent>,
) {
    if opts.no_keyboard {
        tracing::info!("keyboard monitoring disabled (--no-keyboard)");
    } else if opts.impersonate_orca && opts.session_address.is_none() {
        tracing::warn!("keyboard monitoring needs a session bus address; it stays off");
    } else if let (true, Some(session_address)) =
        (opts.impersonate_orca, opts.session_address.clone())
    {
        // ID-1: Mutter only authorises the owner of org.gnome.Orca.KeyboardMonitor. The name is
        // requested without queueing, so a running Orca is never displaced.
        let (sink, keys) = key_channel();
        let (grabs_tx, grabs) = gorca_rt::watch::channel(GrabSet::default());
        let source = Box::new(MutterKeySource::new(session_address));
        let ctx = KeySourceCtx {
            sink,
            grabs,
            shutdown: shutdown.child_token(),
            clock: Clock::System,
        };
        tasks.spawn("keyboard-source", async move {
            if let Err(e) = source.run(ctx).await {
                tracing::warn!(error = %e, "keyboard source ended");
            }
            // Keep the (unused) grab sender alive for as long as the source ran.
            drop(grabs_tx);
        });
        tasks.spawn(
            "key-pump",
            pump_keys(keys, reader_keys, Arc::clone(epoch), speech),
        );
    } else {
        tracing::info!(
            "keyboard monitoring is off: it needs org.gnome.Orca.KeyboardMonitor, which is only \
             requested with --impersonate-orca"
        );
    }
}

/// The AT-SPI connector, supervised: restarted with backoff while the rest keeps running.
pub(crate) fn wire_atspi(
    tasks: &mut TaskSet,
    opts: &Options,
    focused: Arc<FocusedApp>,
    registry: AppRegistrar,
    state: AtspiState,
    shutdown: &CancellationToken,
) {
    tasks.spawn(
        "atspi-connector",
        supervise_connector(
            AtspiOptions {
                a11y_address: opts.a11y_address.clone(),
                session_address: opts.session_address.clone(),
                enable_accessibility: !opts.no_enable,
                register_events: !opts.shadow,
                focused: Some(focused),
                shadow: opts.shadow,
            },
            state,
            registry,
            shutdown.clone(),
        ),
    );
}

/// B-1 PW-10: `--bench-present-log=<path>` (the only way to turn it on) records every node
/// presentation as a JSON line. The object paths come from the connector's path table through
/// [`PathLabels`]; the core never sees them. Passive: unset means nothing happens, and an
/// unwritable file only costs the log.
pub(crate) fn wire_present_log(
    core: gorca_core::Core,
    opts: &Options,
    state: &AtspiState,
) -> gorca_core::Core {
    let Some(path) = &opts.present_log else {
        return core;
    };
    let labels = Arc::new(PathLabels(state.labels()));
    match gorca_core::PresentLog::to_file(path, labels) {
        Ok(log) => core.with_present_log(log),
        Err(e) => {
            tracing::warn!(error = %e, path = %path.display(), "presentation log not opened");
            core
        }
    }
}

/// The connector's `NodeKey` to `(bus, path)` table as the core's presentation-log labeller.
struct PathLabels(gorca_connector_atspi::AtspiLabels);

impl gorca_core::NodeLabel for PathLabels {
    fn label(&self, key: gorca_model::NodeKey) -> Option<(String, String)> {
        self.0
            .lookup(key)
            .map(|(bus, path)| (bus.to_string(), path.to_string()))
    }
}
