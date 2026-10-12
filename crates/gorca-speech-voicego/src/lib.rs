//! voicego native `SpeechClient`: skeleton with the TBD-V1..V6 markers.
//!
//! Responsibility (ADR-0001 sections 8.2 and 11): the **primary** speech backend. The wire is
//! not decided; the `SpeechClient` trait must not change when these hooks are filled in
//! (they are expressed through [`gorca_output_api::SpeechCaps`]):
//!
//! - **TBD-V1 Wire framing.** Binary SSIP framing (`refs/voicego/BINARY_SSIP.md`), a separate
//!   protocol, or text SSIP with extensions?
//! - **TBD-V2 Atomic replace.** Does one command stop the current utterance and start a new one
//!   with no round trip? If not, `atomic_replace = false`.
//! - **TBD-V3 Ids and events.** Client-chosen ids for events and stop? `Started`, `Mark`,
//!   `Ended` in order per id?
//! - **TBD-V4 Audio progress.** First-PCM and played-duration events (`may_refine`, M-12)?
//! - **TBD-V5 Transport and auth.** Unix socket path, discovery, local mTLS?
//! - **TBD-V6 Failover.** *Proposed:* after 3 failed reconnects fall back to SSIP, announce once,
//!   keep probing voicego.
//!
//! Licensing: voicego is AGPL-3.0-or-later. gorca implements the client from the protocol
//! specification and does not vendor or link voicego code.
//!
//! Until the wire is known, [`VoicegoClient`] reports `Down` and refuses every command, so the
//! speech lane exercises its `Disconnected` path and TBD-V6 failover.
//!
//! Allowed dependencies: `gorca-model`, `gorca-rt`, `gorca-output-api`.

use gorca_output_api::{
    ClientHealth, SpeechCaps, SpeechClient, SpeechCmd, SpeechEvent, SubmitError,
};
use gorca_rt::mpsc;

/// Conservative placeholder capabilities until TBD-V2..V4 are answered.
pub fn placeholder_caps() -> SpeechCaps {
    SpeechCaps {
        // TBD-V2
        atomic_replace: false,
        // TBD-V3
        utterance_ids: true,
        max_in_flight: 1,
        index_marks: false,
        // TBD-V4
        audio_progress: false,
        ssml: false,
    }
}

#[derive(Debug)]
pub struct VoicegoClient {
    caps: SpeechCaps,
    events: mpsc::Receiver<SpeechEvent>,
    // Held so the event channel stays open; the transport task will own it (TBD-V1, TBD-V5).
    _event_tx: mpsc::Sender<SpeechEvent>,
}

impl VoicegoClient {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel(128);
        Self {
            caps: placeholder_caps(),
            events: rx,
            _event_tx: tx,
        }
    }
}

impl Default for VoicegoClient {
    fn default() -> Self {
        Self::new()
    }
}

impl SpeechClient for VoicegoClient {
    fn caps(&self) -> &SpeechCaps {
        &self.caps
    }

    fn submit(&self, _cmd: SpeechCmd) -> Result<(), SubmitError> {
        Err(SubmitError::Disconnected)
    }

    fn events(&mut self) -> &mut mpsc::Receiver<SpeechEvent> {
        &mut self.events
    }

    fn health(&self) -> ClientHealth {
        ClientHealth::Down
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skeleton_is_down_and_refuses_commands() {
        let c = VoicegoClient::new();
        assert_eq!(c.health(), ClientHealth::Down);
        assert_eq!(c.submit(SpeechCmd::StopAll), Err(SubmitError::Disconnected));
        assert!(!c.caps().atomic_replace);
    }
}
