//! Command line. Hand-rolled: the surface is a handful of flags and the binary must not grow a
//! parser dependency for them.

use std::path::PathBuf;

use crate::platform::PlatformOverrides;

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

/// Which fill policy the apps run. The product runs [`FillChoice::Prefetch`] (B-1 decision,
/// 2026-10-14); `--bench-fill-policy=r0` is the bake-off baseline.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum FillChoice {
    /// R0: nothing is prefetched; the cache serves revisits. The B-1 baseline.
    R0,
    /// `gorca_mirror::fill::FocusPrefetch`: focus-window prefetch (the adopted arm C).
    /// `--bench-fill-policy=candidate` is the bake-off name for it.
    #[default]
    Prefetch,
}

/// The flags are independent switches mirrored from the command line, not a state machine.
#[allow(clippy::struct_excessive_bools)]
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
    /// Where applications' peer-to-peer sockets live (`XDG_RUNTIME_DIR`): browse mode talks to
    /// a browser over them. Without it browse mode stays off.
    pub runtime_dir: Option<std::path::PathBuf>,
    /// Do not touch `org.a11y.Status` (ID-5).
    pub no_enable: bool,
    /// Extra uids whose speech server is trusted (`--speech-peer-uid`, repeatable). By default
    /// only a server running as our own user is used, because everything read is sent to it.
    pub speech_peer_uids: Vec<u32>,
    /// The settings file. `None`: built-in defaults, nothing is read, watched or saved.
    pub config_path: Option<PathBuf>,
    /// Do not run `gsettings` to learn the desktop's repeat delay, XKB options and clock format.
    pub no_probe: bool,
    /// Desktop facts given from outside (flags and environment), replacing the probe.
    pub platform: PlatformOverrides,
    /// `--bench-present-log=<path>`: record every node presentation as a JSON line (B-1 PW-10).
    /// Only a command-line flag turns it on, never the environment or a config file: the log
    /// holds what the screen reader presents (secret text is redacted, see `PresentLog`).
    /// Passive: no behaviour depends on it.
    pub present_log: Option<PathBuf>,
    /// `GORCA_SHADOW=1`: read-only beside the user's real screen reader (B-1 PW-11): no keyboard
    /// monitor, no Orca identity, no `org.a11y.Status` writes, no event registration, speech
    /// to a null sink, and only the read calls of the connector's allowlist.
    pub shadow: bool,
    /// `--bench-fill-policy=r0|prefetch|candidate`: default is the adopted focus-window prefetch.
    pub fill_policy: FillChoice,
    /// `--topology=...` / `GORCA_TOPOLOGY`: where the lanes run (bake-off B-5). Default: one
    /// multi-threaded runtime, the behaviour before the switch existed.
    pub topology: crate::topology::Arm,
    /// `--topology-report=PATH` / `GORCA_TOPOLOGY_REPORT`: JSON of what was granted to which
    /// lane (`%p` in the path becomes the pid). Passive; benchmarks read it.
    pub topology_report: Option<String>,
    /// `--shell-cache-dump`: let the prefetch policy dump a verified-complete app (gnome-shell)
    /// with one `Cache.GetItems` after the first focus. Off by default until the Shell main-loop
    /// stall of a dump is measured (B-1 judgement, finding 2).
    pub shell_dump: bool,
}

/// `gorca config ...`: tools around the settings file (ADR-0005 7.4, CV-3).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConfigCmd {
    /// Print the diagnostics of a file (the default path when none is given); exit non-zero if
    /// there are any. The only place where a bad file is an error.
    Check(Option<PathBuf>),
    /// Print the built-in defaults as TOML.
    Defaults,
}

// `Options` is parsed once at startup; boxing it would only add an allocation and a deref.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Eq, PartialEq)]
pub enum Parsed {
    Run(Options),
    Config(ConfigCmd),
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
       gorca config check [PATH]   report problems in a settings file (exit 1 if any)
       gorca config defaults       print the default settings

OPTIONS:
    --speech=voicego|ssip|auto   speech backend (default: auto = voicego, then speech-dispatcher)
    --voicego-socket=PATH        voicego socket (default: $XDG_RUNTIME_DIR/voicego.sock)
    --ssip-socket=PATH           speech-dispatcher socket (default: from SPEECHD_ADDRESS or
                                 $XDG_RUNTIME_DIR/speech-dispatcher/speechd.sock)
    --no-keyboard                do not start keyboard monitoring
    --impersonate-orca           own org.gnome.Orca.KeyboardMonitor (never replaces a running Orca)
    --a11y-address=ADDRESS       a11y bus address (default: ask org.a11y.Bus)
    --session-address=ADDRESS    session bus address (testing; default: $DBUS_SESSION_BUS_ADDRESS)
    --speech-peer-uid=UID        also trust a speech server running as UID (repeatable; default:
                                 only our own user)
    --no-enable                  do not set org.a11y.Status IsEnabled (gorca never writes
                                 ScreenReaderEnabled: that would make GNOME autostart Orca)
    --config=PATH                settings file (default: $XDG_CONFIG_HOME/gorca/gorca.toml)
    --no-probe                   do not ask gsettings about the keyboard and clock
    --bench-present-log=PATH     benchmarks only: write the presentation log (absolute PATH, mode
                                 0600, created anew; secret text is redacted)
    --topology=single|lanes|lanes-rt
                                 where the lanes run (bake-off B-5; default: single = one
                                 multi-threaded runtime). lanes: input and speech on their own
                                 threads, workers and a bulk pool apart. lanes-rt: lanes plus
                                 real-time scheduling through rtkit and pinning to the fast cores, with a
                                 fallback to the plain lanes when rtkit refuses
    --topology-report=PATH       write what each lane was granted as JSON (`%p` = pid)
    --bench-fill-policy=r0|prefetch|candidate
                                 the fill policy of every app (default: prefetch; r0 = no
                                 prefetch, the B-1 baseline; candidate = prefetch)
    --shell-cache-dump           let the prefetch policy dump gnome-shell with one Cache.GetItems
                                 (off: the dump stalls the Shell main loop, not yet measured)
    -h, --help                   print this help
";

impl Options {
    /// Fill unset fields from the environment (the seam of the e2e latency harness and of
    /// service managers). Command-line flags win.
    ///
    /// - `GORCA_IMPERSONATE_ORCA=1`: same as `--impersonate-orca`.
    /// - `GORCA_A11Y_BUS_ADDRESS` or `AT_SPI_BUS_ADDRESS`: a11y bus (no
    ///   `org.a11y.Bus.GetAddress` call is made when it is set).
    /// - `DBUS_SESSION_BUS_ADDRESS`, `XDG_RUNTIME_DIR`, `SPEECHD_ADDRESS`, `SPEECHD_SOCKET`:
    ///   the session bus and the default speech sockets.
    /// - `GORCA_SPEECH_BACKEND=voicego|ssip`: backend, unless `--speech` was given.
    /// - `GORCA_SPEECH_SOCKET`: socket of the selected backend instead of its default.
    /// - `GORCA_CONFIG`, `XDG_CONFIG_HOME`, `HOME`: the settings file.
    /// - `GORCA_REPEAT_DELAY_MS`, `GORCA_XKB_OPTIONS`, `GORCA_CLOCK_24H`: desktop facts that
    ///   replace the `gsettings` probe (the e2e harness and tests use them).
    /// - `GORCA_TOPOLOGY`, `GORCA_TOPOLOGY_REPORT`: same as `--topology`, `--topology-report`.
    /// - `GORCA_SHADOW=1`: shadow mode (PW-11).
    #[must_use]
    pub fn with_env(self, get: impl Fn(&str) -> Option<String>) -> Self {
        let mut o = self;
        if o.speech == SpeechMode::Auto {
            match get("GORCA_SPEECH_BACKEND").as_deref() {
                Some("voicego") => o.speech = SpeechMode::Voicego,
                Some("ssip") => o.speech = SpeechMode::Ssip,
                Some(_) | None => {}
            }
        }
        if let Some(sock) = get("GORCA_SPEECH_SOCKET").filter(|s| !s.is_empty()) {
            let path = PathBuf::from(sock);
            match o.speech {
                SpeechMode::Ssip => {
                    o.ssip_socket.get_or_insert(path);
                }
                SpeechMode::Voicego | SpeechMode::Auto => {
                    o.voicego_socket.get_or_insert(path);
                }
            }
        }
        if o.topology == crate::topology::Arm::default()
            && let Some(arm) = get("GORCA_TOPOLOGY").and_then(|v| crate::topology::Arm::parse(&v))
        {
            o.topology = arm;
        }
        if o.topology_report.is_none() {
            o.topology_report = get("GORCA_TOPOLOGY_REPORT").filter(|s| !s.is_empty());
        }
        if get("GORCA_IMPERSONATE_ORCA").is_some_and(|v| v == "1") {
            o.impersonate_orca = true;
        }
        if get("GORCA_SHADOW").is_some_and(|v| v == "1") {
            o.shadow = true;
        }
        // Last, so nothing above can switch it back on: shadow mode never impersonates Orca,
        // never grabs keys and never writes the a11y status.
        if o.shadow {
            o.impersonate_orca = false;
            o.no_keyboard = true;
            o.no_enable = true;
        }
        // Defaults for what is still open. The daemon's `main` is the only place that reads the
        // environment; everything below this point receives resolved values.
        let xdg = get("XDG_RUNTIME_DIR");
        if o.voicego_socket.is_none() {
            o.voicego_socket = gorca_speech_voicego::default_voicego_path(xdg.as_deref());
        }
        if o.ssip_socket.is_none() {
            o.ssip_socket = gorca_speech_ssip::ssip_path_from_env_values(
                get("SPEECHD_ADDRESS").as_deref(),
                get("SPEECHD_SOCKET").as_deref(),
                xdg.as_deref(),
            );
        }
        if o.a11y_address.is_none() {
            o.a11y_address = get("GORCA_A11Y_BUS_ADDRESS")
                .or_else(|| get("AT_SPI_BUS_ADDRESS"))
                .filter(|s| !s.is_empty());
        }
        if o.runtime_dir.is_none() {
            o.runtime_dir = get("XDG_RUNTIME_DIR")
                .filter(|s| s.starts_with('/'))
                .map(std::path::PathBuf::from);
        }
        if o.session_address.is_none() {
            o.session_address = get("DBUS_SESSION_BUS_ADDRESS").filter(|s| !s.is_empty());
        }
        if o.config_path.is_none() {
            o.config_path = get("GORCA_CONFIG")
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .or_else(|| {
                    get("XDG_CONFIG_HOME")
                        .filter(|s| !s.is_empty())
                        .map(|d| PathBuf::from(d).join("gorca/gorca.toml"))
                })
                .or_else(|| {
                    get("HOME")
                        .filter(|s| !s.is_empty())
                        .map(|d| PathBuf::from(d).join(".config/gorca/gorca.toml"))
                });
        }
        if let Some(v) = get("GORCA_REPEAT_DELAY_MS").and_then(|v| v.parse().ok()) {
            o.platform.repeat_delay_ms = Some(v);
        }
        if let Some(v) = get("GORCA_XKB_OPTIONS") {
            o.platform.xkb_options = Some(v);
        }
        if let Some(v) = get("GORCA_CLOCK_24H") {
            o.platform.clock_24h = Some(v != "0");
        }
        o
    }

    /// Everything explicit, nothing from the environment: the constructor tests use, so that a
    /// forgotten field cannot fall back to the real desktop.
    #[must_use]
    pub fn isolated(
        a11y_address: &str,
        session_address: &str,
        voicego_socket: &std::path::Path,
        ssip_socket: &std::path::Path,
    ) -> Self {
        Self {
            no_keyboard: true,
            no_probe: true,
            a11y_address: Some(a11y_address.to_owned()),
            session_address: Some(session_address.to_owned()),
            voicego_socket: Some(voicego_socket.to_owned()),
            ssip_socket: Some(ssip_socket.to_owned()),
            ..Self::default()
        }
    }
}

/// Parse `args` (without the program name).
pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Parsed, CliError> {
    let mut opts = Options::default();
    let mut it = args.into_iter().peekable();
    if it.peek().is_some_and(|a| a == "config") {
        it.next();
        return match (it.next().as_deref(), it.next(), it.next()) {
            (Some("check"), path, None) => {
                Ok(Parsed::Config(ConfigCmd::Check(path.map(PathBuf::from))))
            }
            (Some("defaults"), None, None) => Ok(Parsed::Config(ConfigCmd::Defaults)),
            (Some(other), _, _) => Err(CliError::Unknown(format!("config {other}"))),
            (None, _, _) => Err(CliError::MissingValue("config")),
        };
    }
    while let Some(arg) = it.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f.to_owned(), Some(v.to_owned())),
            _ => (arg.clone(), None),
        };
        if inline.is_some()
            && matches!(
                flag.as_str(),
                "--no-keyboard" | "--impersonate-orca" | "--no-enable" | "--no-probe" | "--help"
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
            "--no-probe" => opts.no_probe = true,
            "--config" => opts.config_path = Some(PathBuf::from(value("--config")?)),
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
            "--speech-peer-uid" => {
                let v = value("--speech-peer-uid")?;
                let uid = v.parse::<u32>().map_err(|_| CliError::BadValue {
                    flag: "--speech-peer-uid",
                    value: v,
                    expected: "a numeric uid",
                })?;
                opts.speech_peer_uids.push(uid);
            }
            "--bench-present-log" => {
                let v = value("--bench-present-log")?;
                let path = PathBuf::from(&v);
                let plain = path.is_absolute()
                    && path
                        .components()
                        .all(|c| !matches!(c, std::path::Component::ParentDir))
                    && path.file_name().is_some();
                if !plain {
                    return Err(CliError::BadValue {
                        flag: "--bench-present-log",
                        value: v,
                        expected: "an absolute path to a file without `..`",
                    });
                }
                opts.present_log = Some(path);
            }
            "--bench-fill-policy" => {
                let v = value("--bench-fill-policy")?;
                opts.fill_policy = match v.as_str() {
                    "r0" => FillChoice::R0,
                    "prefetch" | "candidate" => FillChoice::Prefetch,
                    _ => {
                        return Err(CliError::BadValue {
                            flag: "--bench-fill-policy",
                            value: v,
                            expected: "r0, prefetch or candidate",
                        });
                    }
                };
            }
            "--topology" => {
                let v = value("--topology")?;
                opts.topology = crate::topology::Arm::parse(&v).ok_or(CliError::BadValue {
                    flag: "--topology",
                    value: v,
                    expected: crate::topology::Arm::EXPECTED,
                })?;
            }
            "--topology-report" => opts.topology_report = Some(value("--topology-report")?),
            "--shell-cache-dump" => opts.shell_dump = true,
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
            Parsed::Help | Parsed::Config(_) => panic!("unexpected help or config"),
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
    fn the_presentation_log_needs_the_bench_flag_and_a_plain_absolute_path() {
        let o = opts(&["--bench-present-log=/run/user/1/p.jsonl"]);
        assert_eq!(
            o.present_log.as_deref(),
            Some(std::path::Path::new("/run/user/1/p.jsonl"))
        );
        assert!(opts(&[]).present_log.is_none());
        // The environment never switches it on.
        let env = |k: &str| (k == "GORCA_PRESENT_LOG").then(|| "/tmp/x.jsonl".to_owned());
        assert!(opts(&[]).with_env(env).present_log.is_none());
        for bad in ["rel.jsonl", "/a/../etc/x", "/", ""] {
            assert!(
                matches!(
                    p(&[&format!("--bench-present-log={bad}")]),
                    Err(CliError::BadValue { .. })
                ),
                "{bad}"
            );
        }
    }

    #[test]
    fn the_fill_policy_is_the_prefetch_unless_the_flag_asks_for_r0() {
        assert_eq!(opts(&[]).fill_policy, FillChoice::Prefetch);
        assert!(!opts(&[]).shell_dump);
        assert!(opts(&["--shell-cache-dump"]).shell_dump);
        assert_eq!(
            opts(&["--bench-fill-policy=candidate"]).fill_policy,
            FillChoice::Prefetch
        );
        assert_eq!(
            opts(&["--bench-fill-policy=prefetch"]).fill_policy,
            FillChoice::Prefetch
        );
        assert_eq!(
            opts(&["--bench-fill-policy", "r0"]).fill_policy,
            FillChoice::R0
        );
        // The environment never changes it.
        let env = |k: &str| (k == "GORCA_FILL_POLICY").then(|| "r0".to_owned());
        assert_eq!(opts(&[]).with_env(env).fill_policy, FillChoice::Prefetch);
        assert!(matches!(
            p(&["--bench-fill-policy=zzz"]),
            Err(CliError::BadValue { .. })
        ));
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
    fn environment_fills_only_what_flags_left_open() {
        let env = |k: &str| match k {
            "GORCA_SPEECH_BACKEND" => Some("ssip".to_owned()),
            "GORCA_SPEECH_SOCKET" => Some("/run/e.sock".to_owned()),
            "GORCA_A11Y_BUS_ADDRESS" => Some("unix:path=/e".to_owned()),
            _ => None,
        };
        let o = opts(&[]).with_env(env);
        assert_eq!(o.speech, SpeechMode::Ssip);
        assert_eq!(
            o.ssip_socket.as_deref(),
            Some(std::path::Path::new("/run/e.sock"))
        );
        assert_eq!(o.a11y_address.as_deref(), Some("unix:path=/e"));
        // Flags win.
        let o = opts(&[
            "--speech=voicego",
            "--voicego-socket=/f.sock",
            "--a11y-address=unix:path=/f",
        ])
        .with_env(env);
        assert_eq!(o.speech, SpeechMode::Voicego);
        assert_eq!(
            o.voicego_socket.as_deref(),
            Some(std::path::Path::new("/f.sock"))
        );
        assert_eq!(o.a11y_address.as_deref(), Some("unix:path=/f"));
        // A socket without a backend goes to voicego, the primary.
        let only_sock = |k: &str| (k == "GORCA_SPEECH_SOCKET").then(|| "/s.sock".to_owned());
        assert_eq!(
            opts(&[]).with_env(only_sock).voicego_socket.as_deref(),
            Some(std::path::Path::new("/s.sock"))
        );
    }

    #[test]
    fn config_paths_and_platform_facts_come_from_flags_then_the_environment() {
        let env = |k: &str| match k {
            "XDG_CONFIG_HOME" => Some("/xdg".to_owned()),
            "HOME" => Some("/home/u".to_owned()),
            "GORCA_REPEAT_DELAY_MS" => Some("300".to_owned()),
            "GORCA_XKB_OPTIONS" => Some("grp:caps_toggle".to_owned()),
            "GORCA_CLOCK_24H" => Some("0".to_owned()),
            _ => None,
        };
        let o = opts(&[]).with_env(env);
        assert_eq!(
            o.config_path.as_deref(),
            Some(std::path::Path::new("/xdg/gorca/gorca.toml"))
        );
        assert_eq!(o.platform.repeat_delay_ms, Some(300));
        assert_eq!(o.platform.xkb_options.as_deref(), Some("grp:caps_toggle"));
        assert_eq!(o.platform.clock_24h, Some(false));
        let home_only = |k: &str| (k == "HOME").then(|| "/home/u".to_owned());
        assert_eq!(
            opts(&[]).with_env(home_only).config_path.as_deref(),
            Some(std::path::Path::new("/home/u/.config/gorca/gorca.toml"))
        );
        assert_eq!(
            opts(&["--config=/etc/g.toml"])
                .with_env(env)
                .config_path
                .as_deref(),
            Some(std::path::Path::new("/etc/g.toml"))
        );
        assert!(opts(&["--no-probe"]).no_probe);
        assert!(
            Options::isolated(
                "a",
                "b",
                std::path::Path::new("/v"),
                std::path::Path::new("/s")
            )
            .config_path
            .is_none()
        );
    }

    #[test]
    fn config_subcommands() {
        assert_eq!(
            p(&["config", "check"]),
            Ok(Parsed::Config(ConfigCmd::Check(None)))
        );
        assert_eq!(
            p(&["config", "check", "/x.toml"]),
            Ok(Parsed::Config(ConfigCmd::Check(Some(PathBuf::from(
                "/x.toml"
            )))))
        );
        assert_eq!(
            p(&["config", "defaults"]),
            Ok(Parsed::Config(ConfigCmd::Defaults))
        );
        assert!(p(&["config"]).is_err());
        assert!(p(&["config", "frob"]).is_err());
    }

    #[test]
    fn peer_uids_are_explicit_and_repeatable() {
        assert!(opts(&[]).speech_peer_uids.is_empty(), "same-uid default");
        let o = opts(&["--speech-peer-uid=0", "--speech-peer-uid", "115"]);
        assert_eq!(o.speech_peer_uids, [0, 115]);
        assert!(matches!(
            p(&["--speech-peer-uid=root"]),
            Err(CliError::BadValue { .. })
        ));
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
