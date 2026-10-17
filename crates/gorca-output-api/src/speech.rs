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

/// Which language, output module and synthesis voice the server speaks with (the user's
/// `[speech]` language, output_module and voice). An empty string leaves that item at the
/// server default. Sent at connect and when the settings change.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct VoiceSetup {
    /// BCP-47 or ISO 639 code, for example `ru`.
    pub language: Arc<str>,
    /// Server output module, for example `espeak-ng` or `rhvoice`.
    pub output_module: Arc<str>,
    /// Synthesis voice name.
    pub voice: Arc<str>,
}

impl VoiceSetup {
    /// The SSIP `SET self ...` lines (no line ending). The module comes first because
    /// switching it resets the language and voice on some servers. The last line always turns
    /// the server's own punctuation reading off.
    pub fn ssip_lines(&self) -> Vec<String> {
        // A value with a control character could end the line and start another command.
        let ok = |s: &str| !s.is_empty() && !s.chars().any(char::is_control);
        let mut v = Vec::with_capacity(3);
        if ok(&self.output_module) {
            v.push(format!("SET self OUTPUT_MODULE {}", self.output_module));
        }
        if ok(&self.language) {
            v.push(format!("SET self LANGUAGE {}", self.language));
        }
        if ok(&self.voice) {
            v.push(format!("SET self SYNTHESIS_VOICE {}", self.voice));
        }
        // gorca names every symbol itself (`gorca-speech` symbol tables, per level and
        // language), so the server must not read punctuation a second time.
        v.push("SET self PUNCTUATION none".to_owned());
        v
    }
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
    /// Language, output module and voice.
    Configure(VoiceSetup),
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
