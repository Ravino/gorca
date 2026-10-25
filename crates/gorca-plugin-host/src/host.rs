// SPDX-License-Identifier: GPL-3.0-or-later
//! Host side of the WIT 0.10 `app-module` imports.
//!
//! Every import: checks the permission first (denied = `none`/dropped and counted, not a
//! fault), verifies guest handles with the per-instance MAC, checks lengths before using a
//! value, and only buffers effects. Effects are applied by the caller after a successful return
//! (all or nothing, ADR-0019 I-3). Plugins see the non-`host` API only.

use std::sync::Arc;

use wasmtime::Result;

use crate::handles::{HandleError, HandleMinter};
use crate::limits::{
    Cap, MAX_EFFECT_TEXT_PER_CALL, MAX_FACTS_PER_CALL, MAX_FORGED_PER_CALL,
    MAX_OTHER_EFFECTS_PER_CALL, PluginLimiter, check_len,
};
use crate::trust::Perms;
use crate::view::ModelView;

#[allow(
    clippy::all,
    clippy::pedantic,
    missing_docs,
    missing_debug_implementations,
    unused_qualifications
)]
pub(crate) mod bindings {
    wasmtime::component::bindgen!({
        // The host world of M18a-1: `app-module` without the `present` export, which is
        // `@unstable(feature = present-edit)` until M22b1 freezes PresIr (ADR-0026 M18a-6).
        // wasmtime's bindgen enables every unstable feature, so the world is spelled out here.
        inline: "
            package gorca:plugin-host;
            world rules-host {
                import gorca:ext/model@0.10.0;
                import gorca:ext/facts@0.10.0;
                import gorca:ext/output@0.10.0;
                import gorca:ext/actions@0.10.0;
                import gorca:ext/settings@0.10.0;
                import gorca:ext/timers@0.10.0;
                import gorca:ext/requests@0.10.0;
                import gorca:ext/diag@0.10.0;
                export gorca:ext/rules@0.10.0;
                export gorca:ext/commands@0.10.0;
                export gorca:ext/reactor@0.10.0;
            }
        ",
        world: "rules-host",
        path: "../../wit/gorca-ext-0.10",
        // A host function that runs out of budget traps the guest.
        imports: { default: trappable },
    });
}

use bindings::gorca::ext::{
    actions, diag, facts, model, output, requests, settings, timers, types,
};

/// A fact value, reduced to what admission needs (positions, grids and areas are not kept in
/// this slice).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// No value.
    Absent,
    /// Registry code.
    Code(u16),
    /// Bit set.
    Bits(u64),
    /// Number.
    Number(u32),
    /// Text.
    Text(String),
    /// Boolean.
    Truth(bool),
    /// A structured value not yet decoded.
    Other,
}

/// What a fact targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// A node (host-side index).
    Node(u32),
    /// A manifest selector.
    Selector(u16),
}

/// A buffered effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// `facts.assert`.
    Assert {
        /// Target.
        target: Target,
        /// Group code.
        group: u16,
        /// 0 patch, 1 inferred.
        layer: u8,
        /// Value.
        value: Value,
    },
    /// `facts.retract`.
    Retract {
        /// Target.
        target: Target,
        /// Group.
        group: u16,
    },
    /// `facts.hide` / `unhide`.
    Hide {
        /// Target.
        target: Target,
        /// True for hide.
        hide: bool,
    },
    /// `facts.category`.
    Category {
        /// Target.
        target: Target,
        /// Category.
        cat: u16,
        /// Membership.
        member: bool,
    },
    /// `output.say` (text bytes only; rendering is the presenter's).
    Say {
        /// Parts.
        parts: usize,
    },
    /// `output.cue`.
    Cue(String),
    /// A navigation/action request.
    Act(&'static str),
    /// `diag.log`.
    Log(u8, String),
}

/// Per-instance counters (diagnostics, `explain`).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Counters {
    /// Imports called without the permission.
    pub denied: u64,
    /// Handles with a bad tag.
    pub forged: u64,
    /// Handles with a dead generation.
    pub dead: u64,
    /// Over-cap arguments seen by the host.
    pub oversize: u64,
}

/// Per-call state of one plugin instance.
pub struct HostState {
    /// Effective permissions.
    pub grants: Perms,
    /// Handle minter of this instance.
    pub minter: HandleMinter,
    /// The snapshot being read.
    pub view: Arc<dyn ModelView>,
    /// Memory limiter.
    pub limiter: PluginLimiter,
    /// Buffered effects of the current call.
    pub effects: Vec<Effect>,
    /// Lifetime counters.
    pub counters: Counters,
    /// Set when the call produced a malformed effect: the whole call is rejected.
    pub malformed: bool,
    facts: usize,
    other: usize,
    text_bytes: usize,
    forged_in_call: u32,
}

impl std::fmt::Debug for HostState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostState")
            .field("grants", &self.grants)
            .field("counters", &self.counters)
            .finish_non_exhaustive()
    }
}

impl HostState {
    /// A fresh state.
    #[must_use]
    pub fn new(
        grants: Perms,
        minter: HandleMinter,
        view: Arc<dyn ModelView>,
        limiter: PluginLimiter,
    ) -> Self {
        Self {
            grants,
            minter,
            view,
            limiter,
            effects: Vec::new(),
            counters: Counters::default(),
            malformed: false,
            facts: 0,
            other: 0,
            text_bytes: 0,
            forged_in_call: 0,
        }
    }

    /// Reset the per-call buffers before a guest call.
    pub fn begin_call(&mut self, view: Arc<dyn ModelView>) {
        self.view = view;
        self.effects.clear();
        self.malformed = false;
        self.facts = 0;
        self.other = 0;
        self.text_bytes = 0;
        self.forged_in_call = 0;
        self.limiter.begin_call();
    }

    fn allowed(&mut self, p: Perms) -> bool {
        if self.grants.contains(p) {
            true
        } else {
            self.counters.denied += 1;
            false
        }
    }

    /// Verify a guest handle; a bad one reads `none`, is counted, and 16 in a call reject the
    /// call (§2.3.3).
    fn node(&mut self, h: u64) -> Option<u32> {
        let view = Arc::clone(&self.view);
        match self.minter.verify(h, |i| view.generation(i)) {
            Ok(v) => Some(v.index),
            Err(e) => {
                match e {
                    HandleError::Forged => self.counters.forged += 1,
                    HandleError::Dead => self.counters.dead += 1,
                }
                self.forged_in_call += 1;
                if self.forged_in_call >= MAX_FORGED_PER_CALL {
                    self.malformed = true;
                }
                None
            }
        }
    }

    fn mint(&self, index: u32) -> Option<u64> {
        self.view
            .generation(index)
            .map(|g| self.minter.mint(index, g))
    }

    fn cap(&mut self, cap: Cap, len: usize) -> bool {
        if check_len(cap, len).is_ok() {
            true
        } else {
            self.counters.oversize += 1;
            self.malformed = true;
            false
        }
    }

    fn push_fact(&mut self, e: Effect, text: usize) {
        self.facts += 1;
        self.text_bytes += text;
        if self.facts > MAX_FACTS_PER_CALL || self.text_bytes > MAX_EFFECT_TEXT_PER_CALL {
            self.malformed = true;
            return;
        }
        self.effects.push(e);
    }

    fn push_other(&mut self, e: Effect, text: usize) {
        self.other += 1;
        self.text_bytes += text;
        if self.other > MAX_OTHER_EFFECTS_PER_CALL || self.text_bytes > MAX_EFFECT_TEXT_PER_CALL {
            self.malformed = true;
            return;
        }
        self.effects.push(e);
    }

    fn target(&mut self, t: &facts::FactTarget) -> Option<Target> {
        match t {
            facts::FactTarget::Node(h) => self.node(*h).map(Target::Node),
            // Anchors are minted by the host through `anchor-of`, which returns `none` in this
            // slice, so any anchor the guest passes is forged.
            facts::FactTarget::Anchor(h) => {
                self.counters.forged += 1;
                self.forged_in_call += 1;
                let _ = h;
                None
            }
            facts::FactTarget::Selector(s) => Some(Target::Selector(*s)),
        }
    }
}

impl types::Host for HostState {}

impl model::Host for HostState {
    fn app(&mut self) -> Result<types::AppInfo> {
        Ok(types::AppInfo {
            name: String::new(),
            engine: 0,
            verified: false,
        })
    }
    fn doc_url(&mut self) -> Result<Option<String>> {
        Ok(None)
    }
    fn focus(&mut self) -> Result<Option<u64>> {
        Ok(None)
    }
    fn cursor(&mut self) -> Result<Option<u64>> {
        Ok(None)
    }
    fn role(&mut self, n: u64) -> Result<Option<u16>> {
        Ok(self.node(n).and_then(|i| self.view.role(i)))
    }
    fn prop(&mut self, n: u64, _group: u16) -> Result<Option<types::PropValue>> {
        let _ = self.node(n);
        Ok(None)
    }
    fn name(&mut self, n: u64) -> Result<Option<String>> {
        if !self.allowed(Perms::READ_NAMES) {
            return Ok(None);
        }
        Ok(self.node(n).and_then(|i| self.view.name(i)))
    }
    fn states(&mut self, n: u64) -> Result<u64> {
        Ok(self.node(n).map_or(0, |i| self.view.states(i)))
    }
    fn parent(&mut self, n: u64) -> Result<Option<u64>> {
        Ok(self
            .node(n)
            .and_then(|i| self.view.parent(i))
            .and_then(|p| self.mint(p)))
    }
    fn children(&mut self, n: u64, max: u16) -> Result<Vec<u64>> {
        let Some(i) = self.node(n) else {
            return Ok(Vec::new());
        };
        Ok(self
            .view
            .children(i, max.min(256))
            .into_iter()
            .filter_map(|c| self.mint(c))
            .collect())
    }
    fn subtree(&mut self, root: u64, _d: u8, _m: u16) -> Result<Vec<types::ViewNode>> {
        let _ = self.node(root);
        Ok(Vec::new())
    }
    fn text(&mut self, n: u64, start: u32, len: u32) -> Result<Option<String>> {
        if !self.allowed(Perms::READ_TEXT) {
            return Ok(None);
        }
        Ok(self
            .node(n)
            .and_then(|i| self.view.text(i, start, len.min(64 << 10))))
    }
    fn attr(&mut self, n: u64, key: String) -> Result<Option<String>> {
        if !self.allowed(Perms::READ_ATTRIBUTES) {
            return Ok(None);
        }
        if !self.cap(Cap::Key, key.len()) {
            return Ok(None);
        }
        Ok(self.node(n).and_then(|i| self.view.attr(i, &key)))
    }
    fn matches(&mut self, n: u64, _sel: u16) -> Result<bool> {
        let _ = self.node(n);
        Ok(false)
    }
    fn select(&mut self, root: u64, _sel: u16, _max: u16) -> Result<Vec<u64>> {
        let _ = self.node(root);
        Ok(Vec::new())
    }
    fn anchor_of(&mut self, n: u64, _kind: model::AnchorKind) -> Result<Option<u64>> {
        let _ = self.node(n);
        Ok(None)
    }
    fn prov(&mut self, n: u64, _group: u16) -> Result<Option<types::ProvInfo>> {
        let _ = self.node(n);
        Ok(None)
    }
}

fn value_of(v: &types::PropValue) -> (Value, usize) {
    match v {
        types::PropValue::Absent => (Value::Absent, 0),
        types::PropValue::Code(c) => (Value::Code(*c), 0),
        types::PropValue::Bits(b) => (Value::Bits(*b), 0),
        types::PropValue::Number(n) => (Value::Number(*n), 0),
        types::PropValue::Text(t) => (Value::Text(t.clone()), t.len()),
        types::PropValue::Truth(b) => (Value::Truth(*b), 0),
        types::PropValue::Position(_) | types::PropValue::Grid(_) | types::PropValue::Area(_) => {
            (Value::Other, 0)
        }
    }
}

impl facts::Host for HostState {
    fn assert(
        &mut self,
        target: facts::FactTarget,
        group: u16,
        value: types::PropValue,
        at: facts::Layer,
        _conf: u16,
        _ttl_ms: Option<u32>,
    ) -> Result<()> {
        if !self.allowed(Perms::CONTRIBUTE_FACTS) {
            return Ok(());
        }
        if let types::PropValue::Text(t) = &value
            && !self.cap(Cap::Text, t.len())
        {
            return Ok(());
        }
        let Some(target) = self.target(&target) else {
            return Ok(());
        };
        let (value, tl) = value_of(&value);
        let layer = match at {
            facts::Layer::Patch => 0,
            facts::Layer::Inferred => 1,
        };
        self.push_fact(
            Effect::Assert {
                target,
                group,
                layer,
                value,
            },
            tl,
        );
        Ok(())
    }
    fn retract(&mut self, target: facts::FactTarget, group: u16, _at: facts::Layer) -> Result<()> {
        if self.allowed(Perms::CONTRIBUTE_FACTS)
            && let Some(target) = self.target(&target)
        {
            self.push_fact(Effect::Retract { target, group }, 0);
        }
        Ok(())
    }
    fn hide(&mut self, target: facts::FactTarget) -> Result<()> {
        if self.allowed(Perms::CONTRIBUTE_FACTS)
            && let Some(target) = self.target(&target)
        {
            self.push_fact(Effect::Hide { target, hide: true }, 0);
        }
        Ok(())
    }
    fn unhide(&mut self, target: facts::FactTarget) -> Result<()> {
        if self.allowed(Perms::CONTRIBUTE_FACTS)
            && let Some(target) = self.target(&target)
        {
            self.push_fact(
                Effect::Hide {
                    target,
                    hide: false,
                },
                0,
            );
        }
        Ok(())
    }
    fn category(&mut self, target: facts::FactTarget, cat: u16, member: bool) -> Result<()> {
        if self.allowed(Perms::CONTRIBUTE_FACTS)
            && let Some(target) = self.target(&target)
        {
            self.push_fact(
                Effect::Category {
                    target,
                    cat,
                    member,
                },
                0,
            );
        }
        Ok(())
    }
}

impl output::Host for HostState {
    fn say(&mut self, parts: Vec<output::Part>, _u: output::Urgency) -> Result<()> {
        if !self.allowed(Perms::SPEAK) {
            return Ok(());
        }
        if parts.len() > 16 {
            self.malformed = true;
            return Ok(());
        }
        let mut bytes = 0;
        for p in &parts {
            if let output::Part::Text(types::Text::Literal(s)) = p {
                if !self.cap(Cap::Text, s.len()) {
                    return Ok(());
                }
                bytes += s.len();
            }
        }
        self.push_other(Effect::Say { parts: parts.len() }, bytes);
        Ok(())
    }
    fn cue(&mut self, name: String) -> Result<()> {
        if self.allowed(Perms::SPEAK) && self.cap(Cap::Key, name.len()) {
            let n = name.len();
            self.push_other(Effect::Cue(name), n);
        }
        Ok(())
    }
}

impl actions::Host for HostState {
    // AC-3: acting needs a user. `derive` never acts; the R lane (later slice) enables these
    // while a command of the plugin is in flight. Here they are permission-checked and dropped.
    fn activate(&mut self, n: u64, _a: u16) -> Result<()> {
        let _ = self.node(n);
        let _ = self.allowed(Perms::ACT);
        Ok(())
    }
    fn focus(&mut self, n: u64) -> Result<()> {
        let _ = self.node(n);
        let _ = self.allowed(Perms::ACT);
        Ok(())
    }
    fn move_cursor(&mut self, n: u64) -> Result<()> {
        let _ = self.node(n);
        let _ = self.allowed(Perms::NAVIGATE);
        Ok(())
    }
    fn request_mode(&mut self, _m: actions::Mode) -> Result<()> {
        let _ = self.allowed(Perms::NAVIGATE);
        Ok(())
    }
    fn pointer_click(&mut self, n: u64, _b: u8) -> Result<()> {
        let _ = self.node(n);
        let _ = self.allowed(Perms::POINTER);
        Ok(())
    }
    fn pass_key(&mut self) -> Result<()> {
        Ok(())
    }
}

impl settings::Host for HostState {
    fn get(&mut self, key: String) -> Result<Option<settings::SettingValue>> {
        let _ = self.cap(Cap::Key, key.len());
        Ok(None)
    }
    fn set(&mut self, key: String, _v: settings::SettingValue) -> Result<()> {
        let _ = self.cap(Cap::Key, key.len());
        Ok(())
    }
}

impl timers::Host for HostState {
    fn arm(&mut self, _token: u32, _after_ms: u32) -> Result<()> {
        Ok(())
    }
    fn cancel(&mut self, _token: u32) -> Result<()> {
        Ok(())
    }
}

impl requests::Host for HostState {
    fn request(
        &mut self,
        _q: requests::Query,
        _b: requests::Budget,
    ) -> Result<std::result::Result<u32, requests::RequestError>> {
        Ok(Err(requests::RequestError::Denied))
    }
    fn cancel(&mut self, _id: u32) -> Result<()> {
        Ok(())
    }
}

impl diag::Host for HostState {
    fn log(&mut self, level: u8, msg: String) -> Result<()> {
        if self.cap(Cap::LogMessage, msg.len()) {
            self.push_other(Effect::Log(level, msg), 0);
        }
        Ok(())
    }
    fn now_ms(&mut self) -> Result<Option<u64>> {
        // No clock in the writer lane (determinism, §2.4.5).
        Ok(None)
    }
}
