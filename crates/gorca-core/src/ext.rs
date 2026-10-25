// SPDX-License-Identifier: GPL-3.0-or-later
//! Extension hook points (ADR-0001 section 10, ADR-0008 section 3.3).
//!
//! The core owns the *shape* of the hooks and nothing else: these traits use core types only
//! (`ExtNode`, `NodeEvent`, `Segment`), so the core never links `gorca-ext-host`, wasmtime
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
use gorca_ext_api::terminal::{Ms, TermAction, TermConfig, TermCounters, TermInput, TermSnapshot};
use gorca_input_api::CmdId;
use gorca_model::sem::{Group, SemEntry, SemRole};
use gorca_model::{AppId, AppInfo, NodeEvent, NodeKey, NodeSnapshot, Segment, SegmentKind};

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
    /// Drop the event and say these words instead: they replace the event's own as the fields of
    /// a `PresIr` (`Reason::Ext`), their origin tagged with the rewriting hook.
    Replace(Vec<Segment>),
    /// Route as usual, and ask the app actor to revalidate its mirror.
    ReEvaluate,
}

impl ExtVerdict {
    #[must_use]
    pub fn then(self, next: ExtVerdict) -> ExtVerdict {
        match self {
            ExtVerdict::Pass => next,
            ExtVerdict::Suppress | ExtVerdict::Replace(_) | ExtVerdict::ReEvaluate => self,
        }
    }
}

/// The value of one fused group as an extension reads it (`host.read-group`): the shape the
/// registry row names.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExtGroup {
    Code(u16),
    Bits(u64),
    Number(u32),
    Text(String),
}

/// What an extension sees of a node: the facts of its fused entry, nothing of the connector's
/// snapshot (ADR-0022 section 8.6). Built by the core, so the extension crates never name the
/// legacy carriers. A secret field's text is withheld here, once, for every boundary
/// (`NodeSnapshot::is_sensitive` and the entry's role, the one secrecy question): no name, no
/// description, no range, and every text group reads `None`.
#[derive(Clone, Debug)]
pub struct ExtNode {
    pub key: NodeKey,
    /// The fused role (registry code on the wire).
    pub role: SemRole,
    /// The `flags` group bits of the fused entry.
    pub flags: u64,
    pub name: Option<Arc<str>>,
    pub description: Option<Arc<str>>,
    /// `(current, min, max)` of a range value (a slider, a level bar).
    pub range: Option<(f64, f64, f64)>,
    secret: bool,
    entry: Arc<SemEntry>,
}

impl ExtNode {
    /// The fused role.
    pub fn sem_role(&self) -> SemRole {
        self.role
    }

    /// The node `snap` with its fused `entry`.
    #[must_use]
    pub fn new(snap: &NodeSnapshot, entry: Arc<SemEntry>) -> Self {
        let secret = gorca_present::is_secret_entry(&entry) || snap.is_sensitive();
        Self {
            key: snap.key(),
            role: entry.sem.role,
            flags: u64::from(entry.sem.flags.bits()),
            name: if secret { None } else { entry.name.clone() },
            description: if secret {
                None
            } else {
                entry
                    .sem
                    .text
                    .as_deref()
                    .and_then(|t| t.description.clone())
            },
            range: match &snap.value() {
                Some(gorca_model::Value::Range { current, min, max }) if !secret => {
                    Some((*current, *min, *max))
                }
                Some(gorca_model::Value::Range { .. } | gorca_model::Value::Text(_)) | None => None,
            },
            secret,
            entry,
        }
    }

    /// The node of a snapshot the way the application's own mirror holds it: the attached entry,
    /// else the provisional entry of the source record (PV-1), else an entry with only the name.
    /// The Reader uses [`ExtNode::new`] with the document's entry for a web node.
    #[must_use]
    pub fn from_snapshot(snap: &NodeSnapshot) -> Self {
        let entry = match (&snap.sem().entry, &snap.sem().rec) {
            (Some(e), _) => Arc::clone(e),
            (None, Some(r)) => Arc::new(r.provisional_entry()),
            (None, None) => Arc::new(SemEntry {
                name: snap.name().clone(),
                ..SemEntry::default()
            }),
        };
        Self::new(snap, entry)
    }

    /// Is this a secret field?
    #[must_use]
    pub fn is_secret(&self) -> bool {
        self.secret
    }

    /// One fused group (WIT `read-group`). `None` for a group the entry does not know, a code
    /// this build does not know, a group that has no shape here, and every text group of a secret.
    #[must_use]
    pub fn group(&self, code: u16) -> Option<ExtGroup> {
        let g = Group::from_code(u8::try_from(code).ok()?)?;
        let e = &self.entry;
        if !e.known.has(g) && g != Group::Role {
            return None;
        }
        let s = &e.sem;
        Some(match g {
            Group::Role => ExtGroup::Code(s.role.code()),
            Group::Name => ExtGroup::Text(self.name.as_deref()?.to_owned()),
            Group::Description => ExtGroup::Text(self.description.as_deref()?.to_owned()),
            Group::Check => ExtGroup::Code(u16::from(s.check.code())),
            Group::Press => ExtGroup::Code(u16::from(s.press.code())),
            Group::Expand => ExtGroup::Code(u16::from(s.expand.code())),
            Group::Select => ExtGroup::Code(u16::from(s.select.code())),
            Group::Current => ExtGroup::Code(u16::from(s.current.code())),
            Group::Invalid => ExtGroup::Code(u16::from(s.invalid.code())),
            Group::Popup => ExtGroup::Code(u16::from(s.popup.code())),
            Group::Avail => ExtGroup::Bits(u64::from(s.avail.code())),
            Group::Flags => ExtGroup::Bits(self.flags),
            Group::Level => ExtGroup::Number(u32::from(s.level)),
            Group::Orientation => ExtGroup::Code(u16::from(s.orientation.code())),
            Group::Sort => ExtGroup::Code(u16::from(s.sort.code())),
            Group::SetPos
            | Group::Grid
            | Group::Live
            | Group::RoleDescription
            | Group::Placeholder
            | Group::KeyShortcuts
            | Group::ValueText
            | Group::NameFrom
            | Group::ErrorText => return None,
        })
    }
}

/// The extensions that matched one application, as the hook points see them.
pub trait AppExt: Send + Sync + fmt::Debug {
    /// `on-event` for every extension that matched, in manifest order. Called by the app actor
    /// after the mirror was updated (WE-2: never between the raw event and the dirty mark).
    fn on_event(&self, ev: &NodeEvent) -> ExtVerdict;
    /// `present`: the first extension that answers `Some` wins. `None` also when an extension is
    /// busy, faulted or disabled: the core's own presentation runs.
    fn present(&self, node: &ExtNode, ctx: PresentContext, epoch: u64) -> Option<Vec<Segment>>;
    /// `policy`: the first extension that does not defer wins (ADR-0008 W3). `None` when no
    /// extension has an opinion, is busy, faulted or disabled: the core's own policy (if any)
    /// runs. The default answers `None`.
    fn policy(
        &self,
        _view: &crate::builtin_ext::PolicyView<'_>,
    ) -> Option<crate::builtin_ext::PolicyVerdict> {
        None
    }
    /// Queue `command(local)` of extension `ext`. Returns at once; the answer is spoken through
    /// the [`ExtSink`].
    fn command(&self, ext: &str, local: u32);
    /// Open a terminal policy session for a terminal of this application (ADR-0001 T-2): the
    /// first extension that exports the `terminal` interface. `None` when no extension of this
    /// application is a terminal policy (or it is disabled): the Reader then tracks no screen.
    /// The default answers `None`.
    fn terminal(&self, _cfg: &TermConfig) -> Option<Box<dyn TermSession>> {
        None
    }
}

/// One terminal's policy state machine as the Reader drives it (WIT interface `terminal`, Rust
/// mirror `gorca_ext_api::terminal`). Either an extension-host slot (the WASM component, with
/// its fault policy) or a native build the daemon linked in. A fault is invisible to the Reader:
/// the session re-baselines itself (the screen is read again) and answers nothing meanwhile.
pub trait TermSession: Send + fmt::Debug {
    /// One input at `now` (monotonic milliseconds); actions are appended to `out`.
    fn step(&mut self, now: Ms, input: TermInput, out: &mut Vec<TermAction>);
    fn counters(&self) -> TermCounters;
    /// Did the policy answer the last input? `false` after a fault, a busy or disabled slot: the
    /// Reader then treats typed keys as possibly secret (fail closed) until it answers again.
    fn alive(&self) -> bool {
        true
    }
    /// Is this the core's own stand-in (no policy behind it: it says nothing and claims no
    /// keys)? The Reader replaces it by a real session as soon as one can be opened.
    fn degraded(&self) -> bool {
        false
    }
    /// The screen as last known, for review and the "say current" commands.
    fn snapshot(&self) -> Option<TermSnapshot>;
}

/// Makes terminal sessions without an extension host: the daemon's native build of `ext-vte`
/// (`--terminal-policy=native`, or the fallback when the component is not installed).
pub trait TermProvider: Send + Sync + fmt::Debug {
    fn open(&self, cfg: &TermConfig) -> Box<dyn TermSession>;
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
        let msg = crate::BackgroundMsg::ExtSay {
            ir: Box::new(ext_words_ir(
                [(SegmentKind::Said, text.as_str())],
                VIA_EXT_SAY,
            )),
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
        let _ = self.tx.try_send(crate::BackgroundMsg::ExtSay {
            ir: Box::new(ext_words_ir(
                [(SegmentKind::Said, text.as_str())],
                VIA_EXT_SAY,
            )),
            interrupt,
        });
    }
}

/// The producer tag (`Origin::via`) of words an extension said on its own: `host.speak`, a
/// command's answer, a notice.
pub const VIA_EXT_SAY: u32 = 3;
/// The producer tag of an on-event rewrite.
pub const VIA_EXT_REWRITE: u32 = 4;

/// The IR of words an extension said (`Reason::Ext`): one `TEXT` field per token, each tagged
/// with the producer `via`. There is no node behind it, so the key is the null key.
pub fn ext_words_ir<'a>(
    tokens: impl IntoIterator<Item = (SegmentKind, &'a str)>,
    via: u32,
) -> gorca_present::PresIr {
    let key = NodeKey {
        app: AppId {
            connector: gorca_model::ConnectorId(0),
            serial: 0,
        },
        local: gorca_model::LocalId(0),
    };
    gorca_present::PresIr::new(
        gorca_present::Reason::Ext,
        key,
        0,
        gorca_present::fields_from_tokens(tokens, via)
            .into_iter()
            .collect(),
    )
}

/// Segments to one string (a test helper and the extension host's answer cap).
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod ext_node_tests {
    use gorca_model::sem::{SemRole, SemText};
    use gorca_model::{
        AppId, ConnectorId, Freshness, LocalId, NodeKey, NodeSnapshot, Role, StateSet, Value,
    };

    use super::*;

    fn snap(role: Role) -> NodeSnapshot {
        NodeSnapshot::new(
            NodeKey {
                app: AppId {
                    connector: ConnectorId(1),
                    serial: 1,
                },
                local: LocalId(3),
            },
            Freshness::Defunct,
        )
        .with_name(Some(Arc::from("Volume")))
        .with_description(Some(Arc::from("The level")))
        .with_value(Some(Value::Range {
            current: 0.5,
            min: 0.0,
            max: 1.0,
        }))
        .with_have(gorca_model::PropMask::PRESENT)
        .with_test_source(role, StateSet::empty())
    }

    fn entry(role: SemRole) -> Arc<SemEntry> {
        let mut e = SemEntry::default();
        e.sem.role = role;
        e.name = Some(Arc::from("Volume"));
        e.sem.text = Some(Arc::new(SemText {
            description: Some(Arc::from("The level")),
            ..SemText::default()
        }));
        e.known = e.known.with(Group::Name).with(Group::Description);
        Arc::new(e)
    }

    #[test]
    fn an_ordinary_node_carries_its_facts_and_answers_group_reads() {
        let n = ExtNode::new(&snap(Role::Slider), entry(SemRole::Slider));
        assert_eq!(n.role, SemRole::Slider);
        assert_eq!(n.name.as_deref(), Some("Volume"));
        assert_eq!(n.range, Some((0.5, 0.0, 1.0)));
        assert!(!n.is_secret());
        assert_eq!(n.group(0), Some(ExtGroup::Code(SemRole::Slider.code())));
        assert_eq!(n.group(1), Some(ExtGroup::Text("Volume".to_owned())));
        // An unregistered code and a group with no shape here read nothing.
        assert_eq!(n.group(4000), None);
        assert_eq!(n.group(Group::Grid.code().into()), None);
    }

    #[test]
    fn a_secret_by_any_signal_has_no_text_and_no_range() {
        // The fused role, the native role of the snapshot.
        for (native, fused) in [
            (Role::TextEntry, SemRole::PasswordBox),
            (Role::PasswordEntry, SemRole::TextBox),
        ] {
            let n = ExtNode::new(&snap(native), entry(fused));
            assert!(n.is_secret(), "{native:?} {fused:?}");
            assert!(n.name.is_none() && n.description.is_none() && n.range.is_none());
            assert_eq!(n.group(1), None, "name");
            assert_eq!(n.group(2), None, "description");
            assert!(n.group(0).is_some(), "the role stays readable");
        }
    }
}
