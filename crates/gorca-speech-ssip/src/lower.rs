//! Pure SSIP lowering: command lines, dot-stuffed data blocks, handshake, capabilities.

use gorca_model::Prio;
use gorca_output_api::{SpeechCaps, SpeechCmd, VoiceParams, VoiceRef};

/// S-3: the writer queue holds 4 commands.
pub const WRITER_QUEUE: usize = 4;
/// Event channel bound (section 8.3).
pub const EVENT_QUEUE: usize = 128;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct SsipTransportCaps;

impl SsipTransportCaps {
    /// `atomic_replace = false`, `max_in_flight = 1` (section 8.2). Ids are emulated.
    pub fn caps() -> SpeechCaps {
        SpeechCaps {
            atomic_replace: false,
            utterance_ids: true,
            max_in_flight: 1,
            index_marks: true,
            audio_progress: false,
            ssml: true,
        }
    }
}

/// One item for the socket writer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SsipOut {
    /// A command line, CRLF-terminated.
    Line(String),
    /// The body after `SPEAK`: dot-stuffed and ended by `.` on its own line.
    Data(String),
}

fn priority_name(p: Prio) -> &'static str {
    match p {
        Prio::Interrupt => "important",
        Prio::Message => "message",
        Prio::Text => "text",
        Prio::Progress => "progress",
        Prio::Notification => "notification",
    }
}

/// Dot-stuff and terminate the body (RFC 821 style, as SSIP does). Newlines are normalised
/// to CRLF; lone CR are dropped.
pub fn data_block(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    for line in text.split('\n') {
        let line = line.trim_end_matches('\r');
        if line.starts_with('.') {
            out.push('.');
        }
        out.push_str(line);
        out.push_str("\r\n");
    }
    out.push_str(".\r\n");
    out
}

/// Lower a command to SSIP items, to be written back to back (pipelined, no waiting for acks).
///
/// `prio` is the priority class of the utterance; `SpeechCmd::Speak` does not carry it, so the
/// caller passes the one it recorded for the id.
pub fn lower(cmd: &SpeechCmd, prio: Prio) -> Vec<SsipOut> {
    match cmd {
        SpeechCmd::Speak {
            text, voice, marks, ..
        } => {
            let mut out = Vec::with_capacity(5);
            out.push(SsipOut::Line(format!(
                "SET self PRIORITY {}\r\n",
                priority_name(prio)
            )));
            if let VoiceRef::Named(name) = voice {
                out.push(SsipOut::Line(format!(
                    "SET self SYNTHESIS_VOICE {name}\r\n"
                )));
            }
            if *marks {
                out.push(SsipOut::Line("SET self SSML_MODE on\r\n".to_owned()));
            }
            out.push(SsipOut::Line("SPEAK\r\n".to_owned()));
            out.push(SsipOut::Data(data_block(&text.text)));
            out
        }
        SpeechCmd::Replace { then, .. } => {
            let mut out = vec![SsipOut::Line("STOP self\r\n".to_owned())];
            out.extend(lower(then, prio));
            out
        }
        SpeechCmd::Stop { .. } => vec![SsipOut::Line("STOP self\r\n".to_owned())],
        // CANCEL also discards what the server queued; STOP ends only the current message.
        SpeechCmd::StopAll => vec![SsipOut::Line("CANCEL self\r\n".to_owned())],
        SpeechCmd::Pause => vec![SsipOut::Line("PAUSE self\r\n".to_owned())],
        SpeechCmd::Resume => vec![SsipOut::Line("RESUME self\r\n".to_owned())],
        SpeechCmd::SetParams(VoiceParams {
            rate,
            pitch,
            volume,
        }) => vec![
            SsipOut::Line(format!("SET self RATE {rate}\r\n")),
            SsipOut::Line(format!("SET self PITCH {pitch}\r\n")),
            SsipOut::Line(format!("SET self VOLUME {volume}\r\n")),
        ],
        SpeechCmd::Configure(setup) => setup
            .ssip_lines()
            .into_iter()
            .map(|l| SsipOut::Line(format!("{l}\r\n")))
            .collect(),
    }
}

/// Handshake lines sent once after connecting (`docs/platform/ssip-speech-dispatcher.md`).
pub fn handshake(client_name: &str) -> Vec<SsipOut> {
    vec![
        SsipOut::Line(format!("SET self CLIENT_NAME {client_name}\r\n")),
        SsipOut::Line("SET self NOTIFICATION all on\r\n".to_owned()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use gorca_model::UttId;
    use gorca_output_api::{Verbalized, VoiceSetup};
    use std::sync::Arc;

    fn speak(text: &str) -> SpeechCmd {
        SpeechCmd::Speak {
            id: UttId(1),
            text: Verbalized {
                text: Arc::from(text),
                lang: None,
            },
            voice: VoiceRef::Default,
            marks: false,
        }
    }

    fn lines(out: &[SsipOut]) -> Vec<&str> {
        out.iter()
            .map(|o| match o {
                SsipOut::Line(s) | SsipOut::Data(s) => s.as_str(),
            })
            .collect()
    }

    #[test]
    fn speak_sets_priority_then_speaks_and_sends_the_body() {
        let out = lower(&speak("hello"), Prio::Text);
        assert_eq!(
            lines(&out),
            ["SET self PRIORITY text\r\n", "SPEAK\r\n", "hello\r\n.\r\n"]
        );
    }

    #[test]
    fn replace_is_stop_plus_speak_pipelined() {
        let cmd = SpeechCmd::Replace {
            stop: UttId(1),
            then: Box::new(speak("new")),
        };
        let out = lower(&cmd, Prio::Interrupt);
        assert_eq!(
            lines(&out),
            [
                "STOP self\r\n",
                "SET self PRIORITY important\r\n",
                "SPEAK\r\n",
                "new\r\n.\r\n"
            ]
        );
    }

    #[test]
    fn named_voice_and_marks_are_selected_before_speak() {
        let cmd = SpeechCmd::Speak {
            id: UttId(2),
            text: Verbalized {
                text: Arc::from("x"),
                lang: None,
            },
            voice: VoiceRef::Named(Arc::from("male1")),
            marks: true,
        };
        let out = lower(&cmd, Prio::Message);
        let l = lines(&out);
        assert!(l.contains(&"SET self SYNTHESIS_VOICE male1\r\n"));
        assert!(l.contains(&"SET self SSML_MODE on\r\n"));
        assert_eq!(l[l.len() - 2], "SPEAK\r\n");
    }

    #[test]
    fn control_commands() {
        assert_eq!(
            lines(&lower(&SpeechCmd::Stop { id: UttId(1) }, Prio::Text)),
            ["STOP self\r\n"]
        );
        assert_eq!(
            lines(&lower(&SpeechCmd::StopAll, Prio::Text)),
            ["CANCEL self\r\n"]
        );
        assert_eq!(
            lines(&lower(&SpeechCmd::Pause, Prio::Text)),
            ["PAUSE self\r\n"]
        );
        assert_eq!(
            lines(&lower(&SpeechCmd::Resume, Prio::Text)),
            ["RESUME self\r\n"]
        );
        let p = SpeechCmd::SetParams(VoiceParams {
            rate: -10,
            pitch: 0,
            volume: 50,
        });
        assert_eq!(
            lines(&lower(&p, Prio::Text)),
            [
                "SET self RATE -10\r\n",
                "SET self PITCH 0\r\n",
                "SET self VOLUME 50\r\n"
            ]
        );
    }

    #[test]
    fn configure_sets_module_then_language_then_voice_and_skips_empty() {
        let full = SpeechCmd::Configure(VoiceSetup {
            language: Arc::from("ru"),
            output_module: Arc::from("espeak-ng"),
            voice: Arc::from("ru"),
        });
        assert_eq!(
            lines(&lower(&full, Prio::Text)),
            [
                "SET self OUTPUT_MODULE espeak-ng\r\n",
                "SET self LANGUAGE ru\r\n",
                "SET self SYNTHESIS_VOICE ru\r\n",
                "SET self PUNCTUATION none\r\n"
            ]
        );
        let lang = SpeechCmd::Configure(VoiceSetup {
            language: Arc::from("ru"),
            ..VoiceSetup::default()
        });
        assert_eq!(
            lines(&lower(&lang, Prio::Text)),
            ["SET self LANGUAGE ru\r\n", "SET self PUNCTUATION none\r\n"]
        );
        // gorca verbalises symbols itself: the server never reads punctuation, whatever is set.
        assert_eq!(
            lines(&lower(
                &SpeechCmd::Configure(VoiceSetup::default()),
                Prio::Text
            )),
            ["SET self PUNCTUATION none\r\n"]
        );
    }

    #[test]
    fn body_is_dot_stuffed_and_cannot_end_the_message_early() {
        assert_eq!(data_block(".\nline\r\n.x"), "..\r\nline\r\n..x\r\n.\r\n");
        assert_eq!(data_block(""), "\r\n.\r\n");
    }

    #[test]
    fn priorities_map_to_ssip_names() {
        let names: Vec<_> = [
            Prio::Interrupt,
            Prio::Message,
            Prio::Text,
            Prio::Progress,
            Prio::Notification,
        ]
        .into_iter()
        .map(priority_name)
        .collect();
        assert_eq!(
            names,
            ["important", "message", "text", "progress", "notification"]
        );
    }

    #[test]
    fn caps_match_the_adr() {
        let caps = SsipTransportCaps::caps();
        assert!(!caps.atomic_replace);
        assert_eq!(caps.max_in_flight, 1);
        assert!(caps.utterance_ids);
    }

    #[test]
    fn handshake_enables_notifications() {
        let h = handshake("gorca:gorca:main");
        assert_eq!(lines(&h)[1], "SET self NOTIFICATION all on\r\n");
    }
}
