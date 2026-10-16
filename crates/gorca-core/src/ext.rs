//! Extension hook points (ADR-0001 section 10, ADR-0008 section 3.3).
//!
//! The core owns the *shape* of the hooks and nothing else: these traits use core types only
//! (`NodeSnapshot`, `NodeEvent`, `Segment`), so the core never links `gorca-ext-host`, wasmtime
//! or any extension. `gorca-daemon` wires an implementation (`gorca_ext_host::ExtHost`) through
//! [`crate::Core::with_ext`].
//!
//! ```text
//! app actor  --on_event-->  AppExt   (verdict: pass / suppress / rewrite / re-evaluate)
//! Reader     --present-->   AppExt   (Some(segments) replaces the core presentation)
//! Reader     --command-->   AppExt   (queued to the host's worker; the answer comes back as
//!                                     speech through the ExtSink, never as a return value)
//! ```
//!
//! Rules the hook points keep:
//! - **Never block.** The Reader only `try_lock`s a hook; a busy or faulted extension is the same
//!   as "no extension" (the core presents). Commands never run on the Reader at all.
//! - **Additive.** With no [`ExtSource`] installed nothing here is on any path.
//! - **No names.** The core never branches on an app, product or engine name (WE-1): matching
//!   happens in the host, from [`AppInfo`], through the extension's manifest.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, OnceLock};

use gorca_config::{ChordSpec, ContextName};
use gorca_input_api::CmdId;
use gorca_model::{AppId, AppInfo, NodeEvent, NodeSnapshot, Segment};

use crate::commands::ContextMask;
use crate::present::PresentContext;

/// Extension command ids start here, far above the dense `CoreCmd` discriminants.
pub const EXT_CMD_BASE: u32 = 0x1_0000;

/// What an `on-event` hook decided about one event (composes left to right, first non-`Pass`
/// wins).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExtVerdict {
    /// No opinion: the event is routed as usual.
    Pass,
    /// Drop the event: it is neither routed to the Reader nor spoken.
    Suppress,
    /// Drop the event and speak these segments instead.
    Rewrite(Vec<Segment>),
    /// Route as usual, and ask the app actor to revalidate its mirror.
    ReEvaluate,
}

impl ExtVerdict {
    #[must_use]
    pub fn then(self, next: ExtVerdict) -> ExtVerdict {
        match self {
            ExtVerdict::Pass => next,
            ExtVerdict::Suppress | ExtVerdict::Rewrite(_) | ExtVerdict::ReEvaluate => self,
        }
    }
}

/// The extensions that matched one application, as the hook points see them.
pub trait AppExt: Send + Sync + fmt::Debug {
    /// `on-event` for every extension that matched, in manifest order. Called by the app actor
    /// after the mirror was updated (WE-2: never between the raw event and the dirty mark).
    fn on_event(&self, ev: &NodeEvent) -> ExtVerdict;
    /// `present`: the first extension that answers `Some` wins. `None` also when an extension is
    /// busy, faulted or disabled: the core's own presentation runs.
    fn present(&self, snap: &NodeSnapshot, ctx: PresentContext, epoch: u64)
    -> Option<Vec<Segment>>;
    /// Queue `command(local)` of extension `ext`. Returns at once; the answer is spoken through
    /// the [`ExtSink`].
    fn command(&self, ext: &str, local: u32);
}

/// Where extension speech goes (the Reader's background lane).
pub trait ExtSink: Send + Sync + fmt::Debug {
    /// `interrupt` is true for answers to a user command, false for notices.
    fn speak(&self, text: String, interrupt: bool);
    /// A notice the user must hear exactly once (an extension was disabled). Unlike [`speak`],
    /// an implementation must not drop it when its lane is momentarily full.
    ///
    /// [`speak`]: ExtSink::speak
    fn notice(&self, text: String) {
        self.speak(text, false);
    }
}

/// The host's view for the core: which extensions apply to which application.
pub trait ExtSource: Send + Sync + fmt::Debug {
    /// The application appeared (called by its app actor). `None`: no extension matches.
    fn attach(&self, app: AppId, info: &AppInfo) -> Option<Arc<dyn AppExt>>;
    /// The application is gone.
    fn detach(&self, app: AppId);
    /// Lookup for the Reader. Cheap and lock-light.
    fn for_app(&self, app: AppId) -> Option<Arc<dyn AppExt>>;
    /// Handed once by the core before the first `attach`.
    fn set_sink(&self, sink: Arc<dyn ExtSink>);
}

/// The Reader's background lane as an [`ExtSink`].
#[derive(Debug)]
pub(crate) struct BackgroundSink {
    tx: gorca_rt::mpsc::Sender<crate::BackgroundMsg>,
}

impl BackgroundSink {
    pub(crate) fn new(tx: gorca_rt::mpsc::Sender<crate::BackgroundMsg>) -> Self {
        Self { tx }
    }
}

/// Retry threads for notices that found the lane full; notices are rare, the cap keeps a flood
/// of disabled slots from spawning threads without bound.
const MAX_NOTICE_RETRIES: usize = 4;
static NOTICE_RETRIES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

impl ExtSink for BackgroundSink {
    fn notice(&self, text: String) {
        use std::sync::atomic::Ordering;
        let msg = crate::BackgroundMsg::ExtSpeech {
            text: Arc::from(text),
            interrupt: false,
        };
        let msg = match self.tx.try_send(msg) {
            Ok(()) | Err(gorca_rt::mpsc::error::TrySendError::Closed(_)) => return,
            Err(gorca_rt::mpsc::error::TrySendError::Full(m)) => m,
        };
        // The lane is full: wait for room on a thread of its own, never on the caller's.
        if NOTICE_RETRIES.fetch_add(1, Ordering::AcqRel) >= MAX_NOTICE_RETRIES {
            NOTICE_RETRIES.fetch_sub(1, Ordering::AcqRel);
            tracing::warn!("extension notice dropped: the lane is full and retries are busy");
            return;
        }
        let tx = self.tx.clone();
        #[allow(clippy::disallowed_methods)]
        let spawned = std::thread::Builder::new()
            .name("gorca-ext-notice".to_owned())
            .spawn(move || {
                // `blocking_send` returns once there is room, or when the Reader is gone.
                let _ = tx.blocking_send(msg);
                NOTICE_RETRIES.fetch_sub(1, Ordering::AcqRel);
            });
        if spawned.is_err() {
            NOTICE_RETRIES.fetch_sub(1, Ordering::AcqRel);
        }
    }

    fn speak(&self, text: String, interrupt: bool) {
        // A full lane drops the notice: extension speech is never worth stalling anyone.
        let _ = self.tx.try_send(crate::BackgroundMsg::ExtSpeech {
            text: Arc::from(text),
            interrupt,
        });
    }
}

/// Segments to one string for the speech lane's text path.
pub fn segments_text(segs: &[Segment]) -> String {
    let mut out = String::new();
    for s in segs {
        if s.text.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&s.text);
    }
    out
}

/// One command an extension declares in its manifest.
#[derive(Clone, Debug)]
pub struct ExtCmdSpec {
    /// Stable name, `ext.<extension>.<command>`.
    pub name: String,
    /// The extension that implements it.
    pub ext: String,
    /// The id the extension's `command` hook receives.
    pub local: u32,
    /// Learn-mode description.
    pub help: String,
    pub contexts: ContextMask,
    pub max_press: u8,
    pub interrupts: bool,
}

#[derive(Clone, Debug)]
struct ExtBinding {
    context: ContextName,
    chord: ChordSpec,
    spec: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ExtCmdError {
    #[error("extension command names must start with `ext.` and be lowercase: `{0}`")]
    BadName(String),
    #[error("extension command `{0}` is already registered")]
    Duplicate(String),
    #[error("unknown extension command `{0}`")]
    Unknown(String),
    #[error("bad default chord `{chord}`: {reason}")]
    BadChord { chord: String, reason: &'static str },
    #[error("command `{0}` cannot be bound in that context")]
    WrongContext(String),
    #[error(
        "default chord `{0}` must use the gorca modifier (an extension never grabs a plain key)"
    )]
    NeedsGorcaModifier(String),
}

/// Extension commands merged into the command registry (ADR-0005 3.1, ADR-0001 section 10).
///
/// Core commands stay a closed enum; extension commands live above [`EXT_CMD_BASE`], named
/// `ext.*`. The keymap compiler resolves user-written `ext.*` names through this table and adds
/// each extension's default chords, never overriding a chord the preset or the user already
/// mentioned (a tombstone counts).
#[derive(Clone, Debug, Default)]
pub struct ExtCommands {
    specs: Vec<ExtCmdSpec>,
    by_name: HashMap<String, usize>,
    bindings: Vec<ExtBinding>,
}

static INSTALLED: OnceLock<ExtCommands> = OnceLock::new();

impl ExtCommands {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a command and return its id.
    pub fn register(&mut self, spec: ExtCmdSpec) -> Result<CmdId, ExtCmdError> {
        let ok = spec.name.starts_with("ext.")
            && spec.name.len() > 4
            && spec
                .name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_'));
        if !ok {
            return Err(ExtCmdError::BadName(spec.name));
        }
        if self.by_name.contains_key(&spec.name) {
            return Err(ExtCmdError::Duplicate(spec.name));
        }
        let idx = self.specs.len();
        self.by_name.insert(spec.name.clone(), idx);
        self.specs.push(spec);
        Ok(Self::id_of(idx))
    }

    /// Bind `chord` in `context` by default.
    pub fn bind(
        &mut self,
        context: ContextName,
        chord: &str,
        command: &str,
    ) -> Result<(), ExtCmdError> {
        let parsed = ChordSpec::parse(chord).map_err(|reason| ExtCmdError::BadChord {
            chord: chord.to_owned(),
            reason,
        })?;
        // An extension's default chord is a gorca chord, so it never grabs a key the user types
        // with (passwords included) and never collides with an application shortcut.
        if !parsed.gorca {
            return Err(ExtCmdError::NeedsGorcaModifier(chord.to_owned()));
        }
        let spec = *self
            .by_name
            .get(command)
            .ok_or_else(|| ExtCmdError::Unknown(command.to_owned()))?;
        if !self.specs[spec].contexts.allows(context) {
            return Err(ExtCmdError::WrongContext(command.to_owned()));
        }
        self.bindings.push(ExtBinding {
            context,
            chord: parsed,
            spec,
        });
        Ok(())
    }

    fn id_of(idx: usize) -> CmdId {
        CmdId(EXT_CMD_BASE.saturating_add(u32::try_from(idx).unwrap_or(u32::MAX - EXT_CMD_BASE)))
    }

    pub fn by_name(&self, name: &str) -> Option<(CmdId, &ExtCmdSpec)> {
        let idx = *self.by_name.get(name)?;
        Some((Self::id_of(idx), &self.specs[idx]))
    }

    pub fn get(&self, id: CmdId) -> Option<&ExtCmdSpec> {
        let idx = usize::try_from(id.0.checked_sub(EXT_CMD_BASE)?).ok()?;
        self.specs.get(idx)
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.specs.iter().map(|s| s.name.as_str())
    }

    pub fn len(&self) -> usize {
        self.specs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.specs.is_empty()
    }

    /// Default bindings: `(context, chord, id, spec)`.
    pub fn default_bindings(
        &self,
    ) -> impl Iterator<Item = (ContextName, &ChordSpec, CmdId, &ExtCmdSpec)> {
        self.bindings.iter().map(|b| {
            (
                b.context,
                &b.chord,
                Self::id_of(b.spec),
                &self.specs[b.spec],
            )
        })
    }

    /// Install this registry for the process (once, before settings are compiled). Returns false
    /// if one was installed already.
    pub fn install(self) -> bool {
        INSTALLED.set(self).is_ok()
    }

    /// The installed registry, if any.
    pub fn installed() -> Option<&'static ExtCommands> {
        INSTALLED.get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(name: &str) -> ExtCmdSpec {
        ExtCmdSpec {
            name: name.to_owned(),
            ext: "x".to_owned(),
            local: 7,
            help: "h".to_owned(),
            contexts: ContextMask::ANY,
            max_press: 1,
            interrupts: true,
        }
    }

    #[test]
    fn ids_are_above_the_core_range_and_resolve_back() {
        let mut r = ExtCommands::new();
        let a = r.register(spec("ext.x.a")).unwrap();
        let b = r.register(spec("ext.x.b")).unwrap();
        assert!(a.0 >= EXT_CMD_BASE && b.0 == a.0 + 1);
        assert_eq!(r.get(b).unwrap().name, "ext.x.b");
        assert_eq!(r.by_name("ext.x.a").unwrap().0, a);
        assert!(r.get(CmdId(3)).is_none());
        assert!(r.get(CmdId(EXT_CMD_BASE + 9)).is_none());
    }

    #[test]
    fn names_are_validated() {
        let mut r = ExtCommands::new();
        assert!(matches!(
            r.register(spec("speech.stop")),
            Err(ExtCmdError::BadName(_))
        ));
        assert!(matches!(
            r.register(spec("ext.")),
            Err(ExtCmdError::BadName(_))
        ));
        r.register(spec("ext.a.b")).unwrap();
        assert!(matches!(
            r.register(spec("ext.a.b")),
            Err(ExtCmdError::Duplicate(_))
        ));
    }

    #[test]
    fn bindings_need_a_known_command_and_a_valid_chord() {
        let mut r = ExtCommands::new();
        r.register(spec("ext.a.b")).unwrap();
        r.bind(ContextName::Focus, "gorca+shift+t", "ext.a.b")
            .unwrap();
        assert!(matches!(
            r.bind(ContextName::Focus, "gorca+t", "ext.nope"),
            Err(ExtCmdError::Unknown(_))
        ));
        assert!(matches!(
            r.bind(ContextName::Focus, "", "ext.a.b"),
            Err(ExtCmdError::BadChord { .. })
        ));
        // Hostile defaults: plain keys and bare modifiers-less chords are refused.
        for plain in ["a", "ctrl+a", "f9", "shift+tab", "alt+f4"] {
            assert!(
                matches!(
                    r.bind(ContextName::Focus, plain, "ext.a.b"),
                    Err(ExtCmdError::NeedsGorcaModifier(_))
                ),
                "{plain}"
            );
        }
        assert_eq!(r.default_bindings().count(), 1);
    }

    #[test]
    fn verdicts_compose_left_to_right() {
        assert_eq!(
            ExtVerdict::Pass.then(ExtVerdict::Suppress),
            ExtVerdict::Suppress
        );
        assert_eq!(
            ExtVerdict::ReEvaluate.then(ExtVerdict::Suppress),
            ExtVerdict::ReEvaluate
        );
    }
}
