//! Command line. Hand-rolled: the surface is a handful of flags and the binary must not grow a
//! parser dependency for them.

use std::path::PathBuf;

/// Which speech backends may be used (`--speech`).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum SpeechMode {
    /// voicego first, speech-dispatcher when voicego is unreachable, switching back when it returns.
    #[default]
    Auto,
    /// voicego only: no fallback.
    Voicego,
    /// speech-dispatcher (SSIP) only.
    Ssip,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Options {
    pub speech: SpeechMode,
    /// Do not start keyboard monitoring.
    pub no_keyboard: bool,
    /// Own `org.gnome.Orca.KeyboardMonitor` so that Mutter authorises our key monitor (ID-1). Off
    /// by default: a running Orca is never replaced.
    pub impersonate_orca: bool,
    pub voicego_socket: Option<PathBuf>,
    pub ssip_socket: Option<PathBuf>,
    /// Address of the a11y bus; default is discovery through `org.a11y.Bus`.
    pub a11y_address: Option<String>,
    /// Address of the session bus (testing); default is `DBUS_SESSION_BUS_ADDRESS`.
    pub session_address: Option<String>,
    /// Do not touch `org.a11y.Status` (ID-5).
    pub no_enable: bool,
}

#[derive(Debug, Eq, PartialEq)]
pub enum Parsed {
    Run(Options),
    Help,
}

#[derive(Debug, Eq, PartialEq, thiserror::Error)]
pub enum CliError {
    #[error("unknown argument `{0}`")]
    Unknown(String),
    #[error("`{0}` needs a value")]
    MissingValue(&'static str),
    #[error("invalid value `{value}` for `{flag}` (expected {expected})")]
    BadValue {
        flag: &'static str,
        value: String,
        expected: &'static str,
    },
}

pub const USAGE: &str = "\
gorca - a screen reader for Linux

USAGE: gorca [OPTIONS]

OPTIONS:
    --speech=voicego|ssip|auto   speech backend (default: auto = voicego, then speech-dispatcher)
    --voicego-socket=PATH        voicego socket (default: $XDG_RUNTIME_DIR/voicego.sock)
    --ssip-socket=PATH           speech-dispatcher socket (default: from SPEECHD_ADDRESS or
                                 $XDG_RUNTIME_DIR/speech-dispatcher/speechd.sock)
    --no-keyboard                do not start keyboard monitoring
    --impersonate-orca           own org.gnome.Orca.KeyboardMonitor (never replaces a running Orca)
    --a11y-address=ADDRESS       a11y bus address (default: ask org.a11y.Bus)
    --session-address=ADDRESS    session bus address (testing; default: $DBUS_SESSION_BUS_ADDRESS)
    --no-enable                  do not set org.a11y.Status IsEnabled/ScreenReaderEnabled
    -h, --help                   print this help
";

/// Parse `args` (without the program name).
pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Parsed, CliError> {
    let mut opts = Options::default();
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f.to_owned(), Some(v.to_owned())),
            _ => (arg.clone(), None),
        };
        if inline.is_some()
            && matches!(
                flag.as_str(),
                "--no-keyboard" | "--impersonate-orca" | "--no-enable" | "--help"
            )
        {
            return Err(CliError::Unknown(arg));
        }
        let mut value = |name: &'static str| -> Result<String, CliError> {
            inline
                .clone()
                .or_else(|| it.next())
                .ok_or(CliError::MissingValue(name))
        };
        match flag.as_str() {
            "-h" | "--help" => return Ok(Parsed::Help),
            "--no-keyboard" => opts.no_keyboard = true,
            "--impersonate-orca" => opts.impersonate_orca = true,
            "--no-enable" => opts.no_enable = true,
            "--speech" => {
                let v = value("--speech")?;
                opts.speech = match v.as_str() {
                    "auto" => SpeechMode::Auto,
                    "voicego" => SpeechMode::Voicego,
                    "ssip" => SpeechMode::Ssip,
                    _ => {
                        return Err(CliError::BadValue {
                            flag: "--speech",
                            value: v,
                            expected: "voicego, ssip or auto",
                        });
                    }
                };
            }
            "--voicego-socket" => {
                opts.voicego_socket = Some(PathBuf::from(value("--voicego-socket")?));
            }
            "--ssip-socket" => opts.ssip_socket = Some(PathBuf::from(value("--ssip-socket")?)),
            "--a11y-address" => opts.a11y_address = Some(value("--a11y-address")?),
            "--session-address" => opts.session_address = Some(value("--session-address")?),
            _ => return Err(CliError::Unknown(arg)),
        }
    }
    Ok(Parsed::Run(opts))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(args: &[&str]) -> Result<Parsed, CliError> {
        parse(args.iter().map(|s| (*s).to_owned()))
    }

    fn opts(args: &[&str]) -> Options {
        match p(args).unwrap() {
            Parsed::Run(o) => o,
            Parsed::Help => panic!("unexpected help"),
        }
    }

    #[test]
    fn defaults_are_auto_speech_and_no_impersonation() {
        let o = opts(&[]);
        assert_eq!(o.speech, SpeechMode::Auto);
        assert!(!o.impersonate_orca && !o.no_keyboard && !o.no_enable);
    }

    #[test]
    fn speech_modes_parse_with_equals_and_space() {
        assert_eq!(opts(&["--speech=voicego"]).speech, SpeechMode::Voicego);
        assert_eq!(opts(&["--speech", "ssip"]).speech, SpeechMode::Ssip);
        assert_eq!(opts(&["--speech=auto"]).speech, SpeechMode::Auto);
    }

    #[test]
    fn flags_and_paths() {
        let o = opts(&[
            "--no-keyboard",
            "--impersonate-orca",
            "--voicego-socket=/run/v.sock",
            "--ssip-socket",
            "/run/s.sock",
            "--a11y-address=unix:path=/x",
            "--no-enable",
        ]);
        assert!(o.no_keyboard && o.impersonate_orca && o.no_enable);
        assert_eq!(
            o.voicego_socket.as_deref(),
            Some(std::path::Path::new("/run/v.sock"))
        );
        assert_eq!(
            o.ssip_socket.as_deref(),
            Some(std::path::Path::new("/run/s.sock"))
        );
        assert_eq!(o.a11y_address.as_deref(), Some("unix:path=/x"));
    }

    #[test]
    fn errors_are_specific() {
        assert_eq!(
            p(&["--bogus"]),
            Err(CliError::Unknown("--bogus".to_owned()))
        );
        assert_eq!(p(&["--speech"]), Err(CliError::MissingValue("--speech")));
        assert!(matches!(
            p(&["--speech=festival"]),
            Err(CliError::BadValue {
                flag: "--speech",
                ..
            })
        ));
        assert_eq!(p(&["--help"]), Ok(Parsed::Help));
        // A value-less switch must not swallow `=`-less lookalikes.
        assert_eq!(
            p(&["--no-keyboard=1"]),
            Err(CliError::Unknown("--no-keyboard=1".to_owned()))
        );
    }
}
