use std::sync::Arc;
use std::time::Duration;

use gorca_model::UttId;
use gorca_rt::mpsc;

/// Text after verbalisation (punctuation, symbols, pronunciation dictionary).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Verbalized {
    pub text: Arc<str>,
    pub lang: Option<Arc<str>>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum VoiceRef {
    #[default]
    Default,
    Named(Arc<str>),
}

/// SSIP-style range, -100 to 100.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct VoiceParams {
    pub rate: i8,
    pub pitch: i8,
    pub volume: i8,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SpeechCmd {
    Speak {
        id: UttId,
        text: Verbalized,
        voice: VoiceRef,
        marks: bool,
    },
    /// Atomic if `caps.atomic_replace`, else lowered by the client.
    Replace {
        stop: UttId,
        then: Box<SpeechCmd>,
    },
    /// Stop this utterance only.
    Stop {
        id: UttId,
    },
    StopAll,
    Pause,
    Resume,
    SetParams(VoiceParams),
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum SpeechErr {
    #[error("synthesis failed")]
    Synthesis,
    #[error("unknown voice")]
    Voice,
    #[error("server overloaded")]
    Overload,
    #[error("server error: {0}")]
    Other(Arc<str>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SpeechEvent {
    Accepted(UttId),
    Started(UttId),
    Mark(UttId, u32),
    AudioProgress(UttId, Duration),
    Ended(UttId),
    Stopped(UttId),
    Error(UttId, SpeechErr),
}

/// What a backend can do. The speech lane adapts to these (S-1, section 7.4).
// Five independent capability flags are the ADR's own definition (section 8.2).
#[allow(clippy::struct_excessive_bools)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct SpeechCaps {
    /// One command = stop + speak (no ack wait).
    pub atomic_replace: bool,
    /// Events and `Stop` are addressed by gorca's `UttId`.
    pub utterance_ids: bool,
    /// Server-side queue depth gorca may use (S-1).
    pub max_in_flight: u8,
    pub index_marks: bool,
    /// Played-duration or first-PCM events (needed for `may_refine`, M-12).
    pub audio_progress: bool,
    pub ssml: bool,
}

impl Default for SpeechCaps {
    /// The least capable backend: plain SSIP.
    fn default() -> Self {
        Self {
            atomic_replace: false,
            utterance_ids: false,
            max_in_flight: 1,
            index_marks: false,
            audio_progress: false,
            ssml: false,
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum SubmitError {
    /// The 4-slot writer queue is full (S-3).
    #[error("speech writer queue is full")]
    Full,
    #[error("speech service is disconnected")]
    Disconnected,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ClientHealth {
    Connecting,
    Ready,
    /// Server not reading or never starting (section 8.3).
    Degraded,
    Down,
}

pub trait SpeechClient: Send + 'static {
    fn caps(&self) -> &SpeechCaps;

    /// Never blocks. `Ok` means queued to the writer task, not "spoken".
    fn submit(&self, cmd: SpeechCmd) -> Result<(), SubmitError>;

    /// Server events, drained by the speech lane. Bounded; on overflow `AudioProgress` and
    /// `Mark` coalesce, `Ended` and `Stopped` are never dropped (section 8.3).
    fn events(&mut self) -> &mut mpsc::Receiver<SpeechEvent>;

    fn health(&self) -> ClientHealth;

    /// Ask the client to say goodbye to its server (SSIP: `QUIT`) and stop. Never blocks; the
    /// owner drops the client shortly after (the failover facade waits a bounded flush).
    fn close(&self) {}
}
