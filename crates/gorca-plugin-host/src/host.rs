// SPDX-License-Identifier: GPL-3.0-or-later
//! Host side of the WIT 0.10 `app-module` imports.
//!
//! Every import: checks the permission first (denied = `none`/dropped and counted, not a
//! fault), verifies guest handles with the per-instance MAC, checks lengths before using a
//! value, and only buffers effects. Effects are applied by the caller after a successful return
//! (all or nothing, ADR-0019 I-3). Plugins see the non-`host` API only.

use std::sync::Arc;
use std::time::{Duration, Instant};

use wasmtime::Result;

use crate::handles::{HandleError, HandleMinter};
use crate::limits::{
    Cap, FORGERY_BUDGET, FuelGrant, MAX_EFFECT_TEXT_PER_CALL, MAX_FACTS_PER_CALL,
    MAX_FORGED_PER_CALL, MAX_OTHER_EFFECTS_PER_CALL, MAX_READ_SET, PluginLimiter, WATCHDOG_EVERY,
    WATCHDOG_FACTOR, check_len, cost,
};
use crate::trust::{Perms, TrustTier, tier_may_enter};
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

/// Longest catalog key of a message fact, most arguments, longest argument name.
const MSG_ID_MAX: usize = 64;
const MSG_ARGS_MAX: usize = 8;
const MSG_ARG_NAME_MAX: usize = 32;

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
    /// A message of the plugin's catalog with its arguments, resolved by the writer's side
    /// when the fact is admitted.
    Msg {
        /// Catalog key.
        id: String,
        /// Named arguments.
        args: Vec<(String, String)>,
    },
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
    /// Reads refused because the node is in a system scope and the tier is third-party.
    pub scope_denied: u64,
}

/// Why the host stopped a call from inside an import.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kill {
    /// Host-call fuel exhausted (imports are metered against the grant).
    Budget,
    /// The wall-clock watchdog (4 x grant time) fired.
    Watchdog,
    /// Too many forged handles (per call or lifetime).
    Forgery,
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
    /// Trust tier of the package (system-scope enforcement).
    pub tier: TrustTier,
    /// Set when an import stopped the call.
    pub kill: Option<Kill>,
    /// Fuel the imports charged since the last return to the guest (applied by the call hook).
    pub pending_fuel: u64,
    /// Nodes this call read or was handed (batch, verified handles, minted results): the read
    /// set R of the revision token (ADR-0026 section 2.6.1). Capped; beyond the cap the token
    /// falls back to the scope-wide stamp (more false rejects, never a false accept).
    pub reads: Vec<u32>,
    /// More than [`MAX_READ_SET`] distinct nodes were touched.
    pub reads_overflow: bool,
    /// The deterministic WASI random stream (restarts every call; see `wasi`).
    pub stream: crate::wasi::Stream,
    host_left: u64,
    imports: u32,
    call_start: Instant,
    /// Thread CPU time at the start of the call: the watchdog counts CPU time, so a plugin
    /// thread the host descheduled is not a misbehaving plugin (the ext host learned this).
    pub(crate) cpu_start: Duration,
    pub(crate) probe_t: u64,
    pub(crate) probe_ns: u64,
    pub(crate) probe_n: u64,
    call_limit: Duration,
    forged_lifetime: u32,
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
        tier: TrustTier,
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
            tier,
            kill: None,
            pending_fuel: 0,
            reads: Vec::new(),
            reads_overflow: false,
            stream: crate::wasi::Stream::default(),
            host_left: 0,
            imports: 0,
            call_start: Instant::now(),
            cpu_start: Duration::ZERO,
            probe_t: 0,
            probe_ns: 0,
            probe_n: 0,
            call_limit: Duration::ZERO,
            forged_lifetime: 0,
            facts: 0,
            other: 0,
            text_bytes: 0,
            forged_in_call: 0,
        }
    }

    /// Reset the per-call buffers before a guest call.
    pub fn begin_call(&mut self, view: Arc<dyn ModelView>, grant: FuelGrant) {
        self.view = view;
        self.kill = None;
        self.pending_fuel = 0;
        self.reads.clear();
        self.reads_overflow = false;
        self.stream.restart();
        self.host_left = grant.units;
        self.imports = 0;
        self.call_start = Instant::now();
        self.cpu_start = gorca_rt::sched::thread_cpu_time();
        self.call_limit = grant.wall * WATCHDOG_FACTOR;
        self.effects.clear();
        self.malformed = false;
        self.facts = 0;
        self.other = 0;
        self.text_bytes = 0;
        self.forged_in_call = 0;
        self.limiter.begin_call();
    }

    /// Record `index` in the read set (deduplicated, capped).
    pub fn note_read(&mut self, index: u32) {
        if self.reads_overflow || self.reads.contains(&index) {
            return;
        }
        if self.reads.len() >= MAX_READ_SET {
            self.reads_overflow = true;
        } else {
            self.reads.push(index);
        }
    }

    /// Charge a WASI import like any other host call (units of the host-work meter).
    pub(crate) fn charge_wasi(&mut self, units: u64) -> Result<()> {
        self.charge(units)
    }

    /// Account host work without refusing it (used where the API cannot fail); the next
    /// `charge` stops the call when the grant is spent.
    fn meter(&mut self, units: u64) {
        self.pending_fuel = self.pending_fuel.saturating_add(units);
        self.host_left = self.host_left.saturating_sub(units);
        if self.host_left == 0 && self.kill.is_none() {
            self.kill = Some(Kill::Budget);
        }
    }

    /// Charge an import **before** it does its work. The work is refused (the guest traps) when
    /// it does not fit in what is left of the call's grant, or when the watchdog fires.
    fn charge(&mut self, units: u64) -> Result<()> {
        if self.kill.is_none() && units > self.host_left {
            self.kill = Some(Kill::Budget);
        }
        if self.kill.is_none() {
            self.meter(units);
            self.imports = self.imports.wrapping_add(1);
            if self.imports.is_multiple_of(WATCHDOG_EVERY)
                && (gorca_rt::sched::thread_cpu_time().saturating_sub(self.cpu_start)
                    > self.call_limit
                    // Wall backstop, far above the CPU limit: imports never block, so a call
                    // this slow in wall time is stuck, not descheduled.
                    || self.call_start.elapsed() > self.call_limit * 50)
            {
                self.kill = Some(Kill::Watchdog);
            }
        }
        match self.kill {
            None => Ok(()),
            Some(Kill::Budget) => Err(wasmtime::Error::msg("host-call fuel exhausted")),
            Some(Kill::Watchdog) => Err(wasmtime::Error::msg("host watchdog")),
            Some(Kill::Forgery) => Err(wasmtime::Error::msg("too many forged handles")),
        }
    }

    /// A handle that did not verify. Per call 16 reject the call; the instance lifetime budget
    /// (`FORGERY_BUDGET`) ends the instance (malformed fault, strike, rebuild with a fresh key).
    fn bad_handle(&mut self, forged: bool) {
        if forged {
            self.counters.forged += 1;
            self.forged_lifetime += 1;
        } else {
            self.counters.dead += 1;
        }
        self.forged_in_call += 1;
        if self.forged_in_call >= MAX_FORGED_PER_CALL || self.forged_lifetime > FORGERY_BUDGET {
            self.malformed = true;
            self.kill = Some(Kill::Forgery);
        }
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
        self.meter(cost::HANDLE_CHECK);
        let view = Arc::clone(&self.view);
        match self.minter.verify(h, |i| view.generation(i)) {
            Ok(v) => {
                if self.scope_refused(v.index) {
                    return None;
                }
                self.note_read(v.index);
                Some(v.index)
            }
            Err(e) => {
                self.bad_handle(e == HandleError::Forged);
                None
            }
        }
    }

    /// SS-1..SS-4: a third-party tier never reads a node of a system scope.
    fn scope_refused(&mut self, index: u32) -> bool {
        match self.view.system_scope(index) {
            Some(s) if !tier_may_enter(self.tier, s) => {
                self.counters.scope_denied += 1;
                true
            }
            Some(_) | None => false,
        }
    }

    /// Mint a handle. Indices above 24 bits and system-scope nodes of third-party tiers are not
    /// mintable (a wrapped index would verify as a different node).
    fn mint(&mut self, index: u32) -> Option<u64> {
        if index >= (1 << 24) || self.scope_refused(index) {
            return None;
        }
        let g = self.view.generation(index)?;
        self.note_read(index);
        Some(self.minter.mint(index, g))
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
            facts::FactTarget::Anchor(_) => {
                self.bad_handle(true);
                None
            }
            facts::FactTarget::Selector(s) => Some(Target::Selector(*s)),
        }
    }
}

impl types::Host for HostState {}

impl model::Host for HostState {
    fn app(&mut self) -> Result<types::AppInfo> {
        self.charge(cost::BASE)?;
        let s = self.view.scope();
        Ok(types::AppInfo {
            name: s.name,
            engine: s.engine,
            verified: s.verified,
        })
    }
    fn doc_url(&mut self) -> Result<Option<String>> {
        self.charge(cost::BASE)?;
        Ok(self.view.scope().url)
    }
    fn focus(&mut self) -> Result<Option<u64>> {
        self.charge(cost::BASE)?;
        Ok(None)
    }
    fn cursor(&mut self) -> Result<Option<u64>> {
        self.charge(cost::BASE)?;
        Ok(None)
    }
    fn role(&mut self, n: u64) -> Result<Option<u16>> {
        self.charge(cost::BASE)?;
        Ok(self.node(n).and_then(|i| self.view.role(i)))
    }
    fn prop(&mut self, n: u64, _group: u16) -> Result<Option<types::PropValue>> {
        self.charge(cost::BASE)?;
        let _ = self.node(n);
        Ok(None)
    }
    fn name(&mut self, n: u64) -> Result<Option<String>> {
        self.charge(cost::BASE)?;
        if !self.allowed(Perms::READ_NAMES) {
            return Ok(None);
        }
        self.charge(cost::NAME_PRE)?;
        let r = self.node(n).and_then(|i| self.view.name(i));
        self.meter(
            r.as_ref()
                .map_or(0, |s| s.len() as u64 * cost::PER_TEXT_BYTE),
        );
        Ok(r)
    }
    fn states(&mut self, n: u64) -> Result<u64> {
        self.charge(cost::BASE)?;
        Ok(self.node(n).map_or(0, |i| self.view.states(i)))
    }
    fn parent(&mut self, n: u64) -> Result<Option<u64>> {
        self.charge(cost::BASE)?;
        Ok(self
            .node(n)
            .and_then(|i| self.view.parent(i))
            .and_then(|p| self.mint(p)))
    }
    fn children(&mut self, n: u64, max: u16) -> Result<Vec<u64>> {
        self.charge(cost::BASE)?;
        let max = max.min(256);
        self.charge(u64::from(max) * cost::PER_CHILD)?;
        let Some(i) = self.node(n) else {
            return Ok(Vec::new());
        };
        let kids = self.view.children(i, max);
        Ok(kids.into_iter().filter_map(|c| self.mint(c)).collect())
    }
    fn subtree(&mut self, root: u64, d: u8, m: u16) -> Result<Vec<types::ViewNode>> {
        self.charge(cost::BASE)?;
        let max_depth = d.min(8);
        let max_nodes = usize::from(m).min(256);
        self.charge(max_nodes as u64 * cost::PER_CHILD)?;
        let Some(r) = self.node(root) else {
            return Ok(Vec::new());
        };
        let names = self.grants.contains(Perms::READ_NAMES);
        let texts = self.grants.contains(Perms::READ_TEXT);
        // Breadth first from the root: (index, position of the parent in `out`, depth).
        let mut queue: std::collections::VecDeque<(u32, Option<u16>, u8)> =
            std::collections::VecDeque::new();
        queue.push_back((r, None, 0));
        let mut out: Vec<types::ViewNode> = Vec::new();
        while let Some((i, parent_pos, depth)) = queue.pop_front() {
            if out.len() >= max_nodes {
                break;
            }
            if self.scope_refused(i) {
                continue;
            }
            let Some(handle) = self.mint(i) else { continue };
            let kids = if depth < max_depth {
                self.view.children(i, 256)
            } else {
                Vec::new()
            };
            let name = if names { self.view.name(i) } else { None };
            let text = if texts {
                self.view
                    .text(i, 0, u32::try_from(Cap::Text.bytes()).unwrap_or(u32::MAX))
            } else {
                None
            };
            let bytes = name.as_ref().map_or(0, String::len) + text.as_ref().map_or(0, String::len);
            self.meter(bytes as u64 * cost::PER_TEXT_BYTE + cost::HANDLE_CHECK);
            let pos = u16::try_from(out.len()).unwrap_or(u16::MAX);
            out.push(types::ViewNode {
                node: handle,
                parent_index: parent_pos,
                depth,
                role: self.view.role(i).unwrap_or(0),
                states: self.view.states(i),
                name,
                text,
                child_count: u32::try_from(kids.len()).unwrap_or(u32::MAX),
            });
            for k in kids {
                queue.push_back((k, Some(pos), depth.saturating_add(1)));
            }
            if self.kill.is_some() {
                break;
            }
        }
        self.charge(0)?;
        Ok(out)
    }
    fn text(&mut self, n: u64, start: u32, len: u32) -> Result<Option<String>> {
        self.charge(cost::BASE)?;
        if !self.allowed(Perms::READ_TEXT) {
            return Ok(None);
        }
        let len = len.min(64 << 10);
        self.charge(u64::from(len) * cost::PER_TEXT_BYTE)?;
        Ok(self.node(n).and_then(|i| self.view.text(i, start, len)))
    }
    fn attr(&mut self, n: u64, key: String) -> Result<Option<String>> {
        self.charge(cost::BASE)?;
        if !self.allowed(Perms::READ_ATTRIBUTES) {
            return Ok(None);
        }
        if !self.cap(Cap::Key, key.len()) {
            return Ok(None);
        }
        self.charge(cost::NAME_PRE)?;
        let r = self.node(n).and_then(|i| self.view.attr(i, &key));
        self.meter(
            r.as_ref()
                .map_or(0, |s| s.len() as u64 * cost::PER_TEXT_BYTE),
        );
        Ok(r)
    }
    fn matches(&mut self, n: u64, _sel: u16) -> Result<bool> {
        self.charge(cost::BASE)?;
        let _ = self.node(n);
        Ok(false)
    }
    fn select(&mut self, root: u64, _sel: u16, _max: u16) -> Result<Vec<u64>> {
        self.charge(cost::BASE)?;
        let _ = self.node(root);
        Ok(Vec::new())
    }
    fn anchor_of(&mut self, n: u64, _kind: model::AnchorKind) -> Result<Option<u64>> {
        self.charge(cost::BASE)?;
        let _ = self.node(n);
        Ok(None)
    }
    fn prov(&mut self, n: u64, _group: u16) -> Result<Option<types::ProvInfo>> {
        self.charge(cost::BASE)?;
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
        types::PropValue::Message(m) => (
            Value::Msg {
                id: m.id.clone(),
                args: m.args.clone(),
            },
            m.id.len() + m.args.iter().map(|(k, v)| k.len() + v.len()).sum::<usize>(),
        ),
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
        if let types::PropValue::Message(m) = &value {
            let ok = m.id.len() <= MSG_ID_MAX
                && m.args.len() <= MSG_ARGS_MAX
                && m.args
                    .iter()
                    .all(|(k, v)| k.len() <= MSG_ARG_NAME_MAX && v.len() <= Cap::Text.bytes());
            if !ok {
                self.counters.oversize += 1;
                self.malformed = true;
                return Ok(());
            }
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
        self.charge(cost::BASE)?;
        if self.allowed(Perms::CONTRIBUTE_FACTS)
            && let Some(target) = self.target(&target)
        {
            self.push_fact(Effect::Retract { target, group }, 0);
        }
        Ok(())
    }
    fn hide(&mut self, target: facts::FactTarget) -> Result<()> {
        self.charge(cost::BASE)?;
        if self.allowed(Perms::CONTRIBUTE_FACTS)
            && let Some(target) = self.target(&target)
        {
            self.push_fact(Effect::Hide { target, hide: true }, 0);
        }
        Ok(())
    }
    fn unhide(&mut self, target: facts::FactTarget) -> Result<()> {
        self.charge(cost::BASE)?;
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
        self.charge(cost::BASE)?;
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
        self.charge(cost::BASE)?;
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
        self.charge(cost::BASE)?;
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
        self.charge(cost::BASE)?;
        let _ = self.node(n);
        let _ = self.allowed(Perms::ACT);
        Ok(())
    }
    fn focus(&mut self, n: u64) -> Result<()> {
        self.charge(cost::BASE)?;
        let _ = self.node(n);
        let _ = self.allowed(Perms::ACT);
        Ok(())
    }
    fn move_cursor(&mut self, n: u64) -> Result<()> {
        self.charge(cost::BASE)?;
        let _ = self.node(n);
        let _ = self.allowed(Perms::NAVIGATE);
        Ok(())
    }
    fn request_mode(&mut self, _m: actions::Mode) -> Result<()> {
        self.charge(cost::BASE)?;
        let _ = self.allowed(Perms::NAVIGATE);
        Ok(())
    }
    fn pointer_click(&mut self, n: u64, _b: u8) -> Result<()> {
        self.charge(cost::BASE)?;
        let _ = self.node(n);
        let _ = self.allowed(Perms::POINTER);
        Ok(())
    }
    fn pass_key(&mut self) -> Result<()> {
        self.charge(cost::BASE)?;
        Ok(())
    }
}

impl settings::Host for HostState {
    fn get(&mut self, key: String) -> Result<Option<settings::SettingValue>> {
        self.charge(cost::BASE)?;
        let _ = self.cap(Cap::Key, key.len());
        Ok(None)
    }
    fn set(&mut self, key: String, _v: settings::SettingValue) -> Result<()> {
        self.charge(cost::BASE)?;
        let _ = self.cap(Cap::Key, key.len());
        Ok(())
    }
}

impl timers::Host for HostState {
    fn arm(&mut self, _token: u32, _after_ms: u32) -> Result<()> {
        self.charge(cost::BASE)?;
        Ok(())
    }
    fn cancel(&mut self, _token: u32) -> Result<()> {
        self.charge(cost::BASE)?;
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
        self.charge(cost::BASE)?;
        Ok(())
    }
}

impl diag::Host for HostState {
    fn log(&mut self, level: u8, msg: String) -> Result<()> {
        self.charge(cost::BASE)?;
        if self.cap(Cap::LogMessage, msg.len()) {
            self.push_other(Effect::Log(level, msg), 0);
        }
        Ok(())
    }
    fn now_ms(&mut self) -> Result<Option<u64>> {
        self.charge(cost::BASE)?;
        // No clock in the writer lane (determinism, §2.4.5).
        Ok(None)
    }
}
