// SPDX-License-Identifier: MIT OR Apache-2.0
//! The shared wasmtime host (ADR-0026 sections 2.4, 2.5): engine (fuel only, no epoch, pooling
//! allocator), pre-load with the CD-W1 `.cwasm` cache, instances, the import implementations with
//! caps, pre-charging and the 64th-import watchdog, wall accounting and fault classification.
//!
//! Both arms call [`PluginRuntime::run`]; the arms differ only in *where* it runs.
//!
//! Recorded results of the lazy-parameter check (SPEC 2.5.3, wasmtime 49.0.3):
//! * `bindgen!` lifts `string` / `list` parameters eagerly into `String` / `Vec`, but lifting is
//!   *preceded* by a per-host-call byte budget (`Store::set_hostcall_fuel`): a 15 MiB argument fails
//!   with "too much data is being copied" **before** any allocation or UTF-8 validation. The harness
//!   sets that budget to [`HOSTCALL_BYTES`] and classifies the failure as a malformed effect.
//! * `WasmStr` has no public length accessor in 49.0.3, so a hand-written `func_wrap` would not be
//!   able to cap before validation either; the hostcall budget is the mechanism used.

use crate::budget::Grant;
use crate::model::{Class, FrozenRows, JobId, NodeId, PluginId, Token};
use crate::timing::now_ns;
use sha2::{Digest, Sha256};
use std::cell::Cell;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use wasmtime::component::{Component, HasSelf, Linker};
use wasmtime::{format_err, Result};
use wasmtime::{
    Config, Engine, InstanceAllocationStrategy, PoolingAllocationConfig, Store, StoreLimits,
    StoreLimitsBuilder, Trap,
};

wasmtime::component::bindgen!({
    path: "../wit",
    world: "bakeoff-rules",
    imports: { default: trappable },
});

use gorca::ext::facts::{FactTarget, Layer};
use gorca::ext::types::{
    AnchorRef, AppInfo, GroupCode, NodeRef, PropValue, ProvInfo, RoleCode, SelectorId, ViewNode,
};

/// Bytes a single host call may lift from the guest (strings and lists together).
pub const HOSTCALL_BYTES: usize = 4096;
pub const MEM_CAP: usize = 16 << 20;
pub const FACT_CAP: usize = 256;
pub const OTHER_CAP: usize = 64;
pub const TEXT_CAP: usize = 1024;
pub const LOG_CAP: usize = 256;
pub const KEY_CAP: usize = 64;
pub const FORGED_TOLERANCE: u32 = 8;
pub const GROUP_REFRESH: u16 = 900;
pub const GROUP_LIMIT: u16 = 1024;

// ---- writer-turn guard (no load, no deserialize on a writer turn) -------------------------------

thread_local! {
    static ON_WRITER_TURN: Cell<bool> = const { Cell::new(false) };
}
/// Number of file reads / deserialize / compile calls made while a writer turn was running.
pub static WRITER_TURN_LOADS: AtomicU64 = AtomicU64::new(0);

pub fn set_writer_turn(on: bool) {
    ON_WRITER_TURN.with(|c| c.set(on));
}
pub fn in_writer_turn() -> bool {
    ON_WRITER_TURN.with(|c| c.get())
}

// ---- allocation counter (A3/A4: no host allocation of the guest's size) --------------------------

pub static BIG_ALLOC_MAX: AtomicUsize = AtomicUsize::new(0);
pub const BIG_ALLOC_MIN: usize = 1 << 20;

/// Global allocator wrapper that records the largest single allocation of 1 MiB or more. The
/// binary installs it with `#[global_allocator]`.
pub struct CountingAlloc;
// SAFETY: forwards every call unchanged to the system allocator and only updates an atomic.
unsafe impl std::alloc::GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, l: std::alloc::Layout) -> *mut u8 {
        if l.size() >= BIG_ALLOC_MIN {
            BIG_ALLOC_MAX.fetch_max(l.size(), Ordering::Relaxed);
        }
        std::alloc::System.alloc(l)
    }
    unsafe fn dealloc(&self, p: *mut u8, l: std::alloc::Layout) {
        std::alloc::System.dealloc(p, l)
    }
    unsafe fn alloc_zeroed(&self, l: std::alloc::Layout) -> *mut u8 {
        if l.size() >= BIG_ALLOC_MIN {
            BIG_ALLOC_MAX.fetch_max(l.size(), Ordering::Relaxed);
        }
        std::alloc::System.alloc_zeroed(l)
    }
    unsafe fn realloc(&self, p: *mut u8, l: std::alloc::Layout, n: usize) -> *mut u8 {
        if n >= BIG_ALLOC_MIN {
            BIG_ALLOC_MAX.fetch_max(n, Ordering::Relaxed);
        }
        std::alloc::System.realloc(p, l, n)
    }
}

// ---- faults and effects -------------------------------------------------------------------------

#[derive(
    Copy, Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub enum FaultKind {
    Trap,
    OutOfFuel,
    /// Import pre-charge exceeded the grant.
    PrechargeExhausted,
    Watchdog,
    LimiterDenied,
    MalformedEffect,
    WallOverrunFault,
    ActorLost,
    InstantiateFailed,
}

impl FaultKind {
    pub fn code(self) -> u8 {
        self as u8 + 1
    }
}

#[derive(Debug)]
struct HostTrap(FaultKind, &'static str);
impl std::fmt::Display for HostTrap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "host trap {:?}: {}", self.0, self.1)
    }
}
impl std::error::Error for HostTrap {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    Absent,
    Code(u16),
    Bits(u64),
    Number(u32),
    Text(Box<str>),
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FactEffect {
    pub node: NodeId,
    pub group: u16,
    pub value: Value,
    pub conf: u16,
    pub inferred: bool,
}

// ---- store data ---------------------------------------------------------------------------------

pub struct St {
    limits: StoreLimits,
    rows: Option<Arc<FrozenRows>>,
    token: Token,
    facts: Vec<FactEffect>,
    refreshes: Vec<NodeId>,
    others: u32,
    imports: u32,
    precharge: u64,
    precharge_cap: u64,
    forged: u32,
    dropped: u32,
    start_ns: u64,
    watchdog_ns: u64,
    mac_key: MacKey,
    abort: Option<FaultKind>,
}

/// Per-instance random MAC key. Keyed SipHash-1-3 (`std`'s `RandomState`, whose keys come from the OS
/// RNG and are never exposed), so the cost matches a production keyed MAC. One key per `PluginRuntime`.
#[derive(Clone, Debug)]
pub struct MacKey(std::hash::RandomState);

impl MacKey {
    pub fn random() -> MacKey {
        MacKey(std::hash::RandomState::new())
    }
}

#[inline]
fn mix(key: &MacKey, idx: u32, gen: u8) -> u32 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = key.0.build_hasher();
    h.write_u64(((gen as u64) << 32) | idx as u64);
    h.finish() as u32
}

/// Mint a handle: `mac:32 | gen:8 | idx:24`.
pub fn mint_handle(key: &MacKey, gen: u8, idx: NodeId) -> u64 {
    ((mix(key, idx, gen) as u64) << 32) | ((gen as u64) << 24) | (idx as u64 & 0xFF_FFFF)
}

impl St {
    fn new(mac_key: MacKey) -> St {
        St {
            limits: StoreLimitsBuilder::new()
                .memory_size(MEM_CAP)
                .trap_on_grow_failure(true)
                .build(),
            rows: None,
            token: Token::default(),
            facts: Vec::new(),
            refreshes: Vec::new(),
            others: 0,
            imports: 0,
            precharge: 0,
            precharge_cap: 0,
            forged: 0,
            dropped: 0,
            start_ns: 0,
            watchdog_ns: u64::MAX,
            mac_key,
            abort: None,
        }
    }

    fn trap(&mut self, k: FaultKind, why: &'static str) -> wasmtime::Error {
        self.abort = Some(k);
        wasmtime::Error::new(HostTrap(k, why))
    }

    /// Import pre-charge (B-8) and the 64th-import wall watchdog.
    #[inline]
    fn charge(&mut self, cost: u64) -> wasmtime::Result<()> {
        self.imports += 1;
        self.precharge += cost;
        if self.precharge > self.precharge_cap {
            return Err(self.trap(
                FaultKind::PrechargeExhausted,
                "import pre-charge exceeds the grant",
            ));
        }
        if self.imports % 64 == 0 && now_ns().saturating_sub(self.start_ns) > self.watchdog_ns {
            return Err(self.trap(FaultKind::Watchdog, "4x grant time exceeded in an import"));
        }
        Ok(())
    }

    fn resolve(&mut self, h: NodeRef) -> Option<NodeId> {
        let idx = (h & 0xFF_FFFF) as NodeId;
        let gen = ((h >> 24) & 0xFF) as u8;
        let mac = (h >> 32) as u32;
        let ok = mac == mix(&self.mac_key, idx, gen)
            && self.rows.as_ref().is_some_and(|r| r.contains(idx));
        if !ok {
            self.forged += 1;
            return None;
        }
        Some(idx)
    }

    fn check_forged(&mut self) -> wasmtime::Result<()> {
        if self.forged > FORGED_TOLERANCE {
            return Err(self.trap(
                FaultKind::MalformedEffect,
                "forged handles beyond tolerance",
            ));
        }
        Ok(())
    }

    fn handle(&self, n: NodeId) -> u64 {
        mint_handle(&self.mac_key, 1, n)
    }

    fn read(
        &mut self,
        n: NodeRef,
        class: Class,
        cost: u64,
    ) -> wasmtime::Result<Option<(NodeId, Arc<FrozenRows>)>> {
        self.charge(cost)?;
        let Some(id) = self.resolve(n) else {
            self.check_forged()?;
            return Ok(None);
        };
        self.token.note_read(id, class);
        Ok(self.rows.clone().map(|r| (id, r)))
    }
}

impl gorca::ext::types::Host for St {}

impl gorca::ext::model::Host for St {
    fn app(&mut self) -> wasmtime::Result<AppInfo> {
        self.charge(30)?;
        Ok(AppInfo {
            name: "bakeoff".into(),
            engine: 1,
            verified: true,
        })
    }
    fn doc_url(&mut self) -> wasmtime::Result<Option<String>> {
        self.charge(30)?;
        Ok(None)
    }
    fn focus(&mut self) -> wasmtime::Result<Option<NodeRef>> {
        self.charge(20)?;
        Ok(None)
    }
    fn cursor(&mut self) -> wasmtime::Result<Option<NodeRef>> {
        self.charge(20)?;
        Ok(None)
    }
    fn role(&mut self, n: NodeRef) -> wasmtime::Result<Option<RoleCode>> {
        Ok(self
            .read(n, Class::Core, 30)?
            .and_then(|(id, r)| r.get(id).map(|x| x.role)))
    }
    fn prop(&mut self, n: NodeRef, group: GroupCode) -> wasmtime::Result<Option<PropValue>> {
        let class = if group == 9 { Class::Geom } else { Class::Core };
        Ok(self
            .read(n, class, 40)?
            .and_then(|(id, r)| r.get(id).map(|x| PropValue::Code(x.role ^ group))))
    }
    fn name(&mut self, n: NodeRef) -> wasmtime::Result<Option<String>> {
        let Some((id, r)) = self.read(n, Class::Names, 40)? else {
            return Ok(None);
        };
        let out = r.get(id).map(|x| x.name.to_string());
        if let Some(s) = &out {
            self.charge(s.len() as u64 / 16)?;
        }
        Ok(out)
    }
    fn states(&mut self, n: NodeRef) -> wasmtime::Result<u64> {
        Ok(self
            .read(n, Class::Core, 30)?
            .and_then(|(id, r)| r.get(id).map(|x| x.states))
            .unwrap_or(0))
    }
    fn parent(&mut self, n: NodeRef) -> wasmtime::Result<Option<NodeRef>> {
        let Some((id, r)) = self.read(n, Class::Struct, 30)? else {
            return Ok(None);
        };
        Ok(r.get(id)
            .and_then(|x| x.parent)
            .filter(|p| r.contains(*p))
            .map(|p| self.handle(p)))
    }
    fn children(&mut self, n: NodeRef, max: u16) -> wasmtime::Result<Vec<NodeRef>> {
        let Some((id, r)) = self.read(n, Class::Struct, 40)? else {
            return Ok(Vec::new());
        };
        let kids: Vec<NodeRef> = r
            .get(id)
            .map(|x| {
                x.children
                    .iter()
                    .filter(|c| r.contains(**c))
                    .take(max as usize)
                    .map(|c| self.handle(*c))
                    .collect()
            })
            .unwrap_or_default();
        self.charge(kids.len() as u64 * 10)?;
        Ok(kids)
    }
    fn subtree(
        &mut self,
        root: NodeRef,
        max_depth: u8,
        max_nodes: u16,
    ) -> wasmtime::Result<Vec<ViewNode>> {
        let Some((id, r)) = self.read(root, Class::Struct, 100)? else {
            return Ok(Vec::new());
        };
        let mut out: Vec<ViewNode> = Vec::new();
        let mut stack: Vec<(NodeId, Option<u16>, u8)> = vec![(id, None, 0)];
        while let Some((cur, parent_index, depth)) = stack.pop() {
            if out.len() >= max_nodes.min(256) as usize {
                break;
            }
            let Some(row) = r.get(cur) else { continue };
            let idx = out.len() as u16;
            self.token.note_read(cur, Class::Core);
            self.token.note_read(cur, Class::Names);
            out.push(ViewNode {
                node: self.handle(cur),
                parent_index,
                depth,
                role: row.role,
                states: row.states,
                name: Some(row.name.to_string()),
                text: None,
                child_count: row.children.len() as u32,
            });
            if depth < max_depth.min(8) {
                for c in row.children.iter().rev() {
                    stack.push((*c, Some(idx), depth + 1));
                }
            }
        }
        // Pre-charge for the work done (B-8): the heaviest import of the subset.
        self.charge(out.len() as u64 * 60)?;
        Ok(out)
    }
    fn text(&mut self, n: NodeRef, start: u32, len: u32) -> wasmtime::Result<Option<String>> {
        let Some((id, r)) = self.read(n, Class::Text, 60)? else {
            return Ok(None);
        };
        let len = len.min(64 * 1024) as usize;
        Ok(r.get(id).map(|x| {
            let b = x.text.as_bytes();
            let s = (start as usize).min(b.len());
            let e = (s + len).min(b.len());
            String::from_utf8_lossy(&b[s..e]).into_owned()
        }))
    }
    fn attr(&mut self, n: NodeRef, key: String) -> wasmtime::Result<Option<String>> {
        if key.len() > KEY_CAP {
            return Err(self.trap(FaultKind::MalformedEffect, "attr key over cap"));
        }
        Ok(self.read(n, Class::Attrs, 50)?.and_then(|(id, r)| {
            r.get(id).and_then(|x| {
                if key == "class" {
                    Some(x.class_attr.to_string())
                } else {
                    None
                }
            })
        }))
    }
    fn matches(&mut self, n: NodeRef, sel: SelectorId) -> wasmtime::Result<bool> {
        Ok(self
            .read(n, Class::Core, 40)?
            .and_then(|(id, r)| r.get(id).map(|x| (x.role as u32) % 3 == sel as u32 % 3))
            .unwrap_or(false))
    }
    fn select(
        &mut self,
        root: NodeRef,
        _sel: SelectorId,
        max: u16,
    ) -> wasmtime::Result<Vec<NodeRef>> {
        let Some((id, r)) = self.read(root, Class::Struct, 100)? else {
            return Ok(Vec::new());
        };
        let v: Vec<NodeRef> = r
            .get(id)
            .map(|x| {
                x.children
                    .iter()
                    .filter(|c| r.contains(**c))
                    .take(max as usize)
                    .map(|c| self.handle(*c))
                    .collect()
            })
            .unwrap_or_default();
        self.charge(v.len() as u64 * 30)?;
        Ok(v)
    }
    fn anchor_of(
        &mut self,
        n: NodeRef,
        _kind: gorca::ext::model::AnchorKind,
    ) -> wasmtime::Result<Option<AnchorRef>> {
        Ok(self.read(n, Class::Struct, 40)?.map(|(id, _)| id as u64))
    }
    fn prov(&mut self, _n: NodeRef, _group: GroupCode) -> wasmtime::Result<Option<ProvInfo>> {
        self.charge(30)?;
        Ok(None)
    }
}

impl gorca::ext::facts::Host for St {
    fn assert(
        &mut self,
        target: FactTarget,
        group: GroupCode,
        value: PropValue,
        at: Layer,
        conf: u16,
        _ttl_ms: Option<u32>,
    ) -> wasmtime::Result<()> {
        self.charge(60)?;
        if self.facts.len() >= FACT_CAP {
            return Err(self.trap(FaultKind::MalformedEffect, "B-6: more than 256 facts"));
        }
        if group >= GROUP_LIMIT {
            return Err(self.trap(FaultKind::MalformedEffect, "unregistered group code"));
        }
        let v = match value {
            PropValue::Absent => Value::Absent,
            PropValue::Code(c) => Value::Code(c),
            PropValue::Bits(b) => Value::Bits(b),
            PropValue::Number(n) => Value::Number(n),
            PropValue::Text(t) => {
                if t.len() > TEXT_CAP {
                    return Err(self.trap(FaultKind::MalformedEffect, "fact text over cap"));
                }
                Value::Text(t.into())
            }
            _ => Value::Other,
        };
        let FactTarget::Node(h) = target else {
            self.dropped += 1; // anchor / selector targets are not part of the subset's fixture
            return Ok(());
        };
        let Some(node) = self.resolve(h) else {
            self.dropped += 1;
            self.check_forged()?;
            return Ok(());
        };
        if group == GROUP_REFRESH {
            self.refreshes.push(node);
            return Ok(());
        }
        self.facts.push(FactEffect {
            node,
            group,
            value: v,
            conf,
            inferred: matches!(at, Layer::Inferred),
        });
        Ok(())
    }
    fn retract(
        &mut self,
        _target: FactTarget,
        _group: GroupCode,
        _at: Layer,
    ) -> wasmtime::Result<()> {
        self.charge(40)?;
        self.other()
    }
    fn hide(&mut self, _target: FactTarget) -> wasmtime::Result<()> {
        self.charge(40)?;
        self.other()
    }
    fn unhide(&mut self, _target: FactTarget) -> wasmtime::Result<()> {
        self.charge(40)?;
        self.other()
    }
    fn category(&mut self, _target: FactTarget, _cat: u16, _member: bool) -> wasmtime::Result<()> {
        self.charge(40)?;
        self.other()
    }
}

impl St {
    fn other(&mut self) -> wasmtime::Result<()> {
        self.others += 1;
        if self.others as usize > OTHER_CAP {
            return Err(self.trap(FaultKind::MalformedEffect, "more than 64 non-fact effects"));
        }
        Ok(())
    }
}

impl gorca::ext::diag::Host for St {
    fn log(&mut self, _level: u8, msg: String) -> wasmtime::Result<()> {
        self.charge(40)?;
        if msg.len() > LOG_CAP {
            return Err(self.trap(FaultKind::MalformedEffect, "log message over cap"));
        }
        self.dropped += 1; // rate limited sink: counted, not stored
        Ok(())
    }
    fn now_ms(&mut self) -> wasmtime::Result<Option<u64>> {
        self.charge(20)?;
        Ok(None) // writer lane: output must not depend on time
    }
}

// ---- engine, pre-load, cache --------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct EngineCfg {
    pub cache_dir: PathBuf,
    pub pooling: bool,
}

pub struct HostEngine {
    pub engine: Engine,
    pub compat_hash: String,
    pub cache_dir: PathBuf,
}

pub struct LoadedPlugin {
    pub name: String,
    pub sha256: String,
    pub cwasm_path: PathBuf,
    pub cwasm_bytes: usize,
    pub load_ns: u64,
    pub cache_hit: bool,
    pre: BakeoffRulesPre<St>,
}

impl HostEngine {
    pub fn new(cfg: &EngineCfg) -> Result<Arc<HostEngine>> {
        let mut c = Config::new();
        c.consume_fuel(true); // fuel only; no epoch interruption (ADR-0026 2.5.2)
        if cfg.pooling {
            let mut p = PoolingAllocationConfig::default();
            p.total_component_instances(512);
            p.total_core_instances(2048);
            p.total_memories(2048);
            p.total_tables(2048);
            p.max_memory_size(32 << 20);
            c.allocation_strategy(InstanceAllocationStrategy::Pooling(p));
        }
        let engine = Engine::new(&c)?;
        let mut h = std::collections::hash_map::DefaultHasher::new();
        engine.precompile_compatibility_hash().hash(&mut h);
        std::fs::create_dir_all(&cfg.cache_dir)?;
        Ok(Arc::new(HostEngine {
            engine,
            compat_hash: format!("{:016x}", h.finish()),
            cache_dir: cfg.cache_dir.clone(),
        }))
    }

    /// Cache file for a package: `<sha256>-<compat hash>.cwasm` (CD-W1 key).
    pub fn cache_path(&self, sha256: &str) -> PathBuf {
        self.cache_dir
            .join(format!("{sha256}-{}.cwasm", self.compat_hash))
    }

    /// Pre-load. Reads the `.cwasm` into a buffer and deserializes **from the buffer** (never
    /// `deserialize_file`). A failed deserialize is a cache miss, not a strike (CD-W2). Must not
    /// run on a writer turn (counted in [`WRITER_TURN_LOADS`]).
    pub fn load(&self, name: &str, wasm: &Path) -> Result<LoadedPlugin> {
        if in_writer_turn() {
            WRITER_TURN_LOADS.fetch_add(1, Ordering::Relaxed);
        }
        let t = now_ns();
        let bytes = std::fs::read(wasm).map_err(|e| format_err!("read {wasm:?}: {e}"))?;
        let sha = hex(&Sha256::digest(&bytes));
        let path = self.cache_path(&sha);
        let mut hit = false;
        let mut component: Option<Component> = None;
        let mut cwasm_bytes = 0;
        if let Ok(buf) = std::fs::read(&path) {
            cwasm_bytes = buf.len();
            // SAFETY: the buffer was produced by `Component::serialize` of this same engine
            // configuration (the file name carries the compatibility hash); a wrong or corrupt
            // file makes `deserialize` return Err, which we treat as a cache miss.
            if let Ok(c) = unsafe { Component::deserialize(&self.engine, &buf) } {
                component = Some(c);
                hit = true;
            }
        }
        let component = match component {
            Some(c) => c,
            None => {
                let c = Component::new(&self.engine, &bytes)?;
                let ser = c.serialize()?;
                cwasm_bytes = ser.len();
                let tmp = path.with_extension(format!(
                    "tmp{}-{}",
                    std::process::id(),
                    crate::timing::gettid()
                ));
                let tmp = tmp.with_file_name(format!(
                    "{}.{}",
                    path.file_name().and_then(|n| n.to_str()).unwrap_or("x"),
                    now_ns()
                ));
                std::fs::write(&tmp, &ser)?;
                std::fs::rename(&tmp, &path)?;
                c
            }
        };
        // Imports outside the subset (wasi clocks/io pulled in by std) trap if called: the host
        // links only what the WIT world needs (ADR-0008).
        let mut linker = Linker::<St>::new(&self.engine);
        BakeoffRules::add_to_linker::<St, HasSelf<St>>(&mut linker, |s| s)?;
        linker.define_unknown_imports_as_traps(&component)?;
        let pre = BakeoffRulesPre::new(linker.instantiate_pre(&component)?)?;
        Ok(LoadedPlugin {
            name: name.to_string(),
            sha256: sha,
            cwasm_path: path,
            cwasm_bytes,
            load_ns: now_ns() - t,
            cache_hit: hit,
            pre,
        })
    }
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

// ---- instances and calls ------------------------------------------------------------------------

/// One rules call's inputs.
pub struct CallSpec<'a> {
    pub rows: Arc<FrozenRows>,
    pub nodes: &'a [NodeId],
    pub interactive: bool,
    pub grant: Grant,
    /// Overrun threshold basis: `grant.ns` (grant / kappa_p).
    pub epoch: u64,
}

#[derive(Debug)]
pub struct CallResult {
    pub outcome: Result<(), FaultKind>,
    pub facts: Vec<FactEffect>,
    pub refreshes: Vec<NodeId>,
    pub token: Token,
    pub exec_ns: u64,
    pub fuel_used: u64,
    pub imports: u32,
    pub precharge: u64,
    pub wall_overrun: bool,
    pub forged: u32,
    pub dropped: u32,
}

pub struct GuestInstance {
    store: Store<St>,
    bindings: BakeoffRules,
    pub instantiate_ns: u64,
}

impl GuestInstance {
    pub fn new(lp: &LoadedPlugin, mac_key: MacKey) -> Result<GuestInstance> {
        let t = now_ns();
        let mut store = Store::new(lp.pre.engine(), St::new(mac_key));
        store.limiter(|s| &mut s.limits);
        store.set_hostcall_fuel(HOSTCALL_BYTES);
        store.set_fuel(1_000_000)?;
        let bindings = lp.pre.instantiate(&mut store)?;
        Ok(GuestInstance {
            store,
            bindings,
            instantiate_ns: now_ns() - t,
        })
    }

    /// Run `attach` once (background budget, not part of any interactive stall).
    pub fn attach(&mut self, fuel: u64) -> Result<()> {
        self.store.set_fuel(fuel)?;
        self.store.data_mut().precharge_cap = u64::MAX;
        let ctx = exports::gorca::ext::rules::RuleCtx {
            epoch: 0,
            interactive: false,
        };
        self.bindings
            .gorca_ext_rules()
            .call_attach(&mut self.store, ctx)?;
        Ok(())
    }

    pub fn derive(&mut self, spec: &CallSpec<'_>) -> CallResult {
        let st = self.store.data_mut();
        st.facts.clear();
        st.refreshes.clear();
        st.others = 0;
        st.imports = 0;
        st.precharge = 0;
        st.precharge_cap = spec.grant.fuel;
        st.forged = 0;
        st.dropped = 0;
        st.abort = None;
        let mut token = Token::new(spec.rows.t);
        for n in spec.nodes {
            if let Some(r) = spec.rows.get(*n) {
                token.bind.push((*n, r.bind_gen));
            }
        }
        st.token = token;
        st.rows = Some(spec.rows.clone());
        let key = st.mac_key.clone();
        let batch: Vec<exports::gorca::ext::rules::Dirty> = spec
            .nodes
            .iter()
            .map(|n| exports::gorca::ext::rules::Dirty {
                node: mint_handle(&key, 1, *n),
                watch: 0,
                cause: 2,
            })
            .collect();
        let ctx = exports::gorca::ext::rules::RuleCtx {
            epoch: spec.epoch,
            interactive: spec.interactive,
        };
        let _ = self.store.set_fuel(spec.grant.fuel);
        let st = self.store.data_mut();
        st.watchdog_ns = spec.grant.ns.saturating_mul(4).max(200_000);
        let t0 = now_ns();
        st.start_ns = t0;
        let r = self
            .bindings
            .gorca_ext_rules()
            .call_derive(&mut self.store, ctx, &batch);
        let exec_ns = now_ns() - t0;
        let left = self.store.get_fuel().unwrap_or(0);
        let fuel_used = spec.grant.fuel.saturating_sub(left);
        let st = self.store.data_mut();
        let outcome = match r {
            Ok(()) => Ok(()),
            Err(e) => Err(classify(&e, st.abort)),
        };
        let g = spec.grant.ns;
        let overrun_limit = (2 * g).max(g + 50_000);
        let wall_overrun = outcome.is_ok() && exec_ns > overrun_limit;
        let ok = outcome.is_ok();
        CallResult {
            outcome,
            facts: if ok {
                std::mem::take(&mut st.facts)
            } else {
                Vec::new()
            },
            refreshes: if ok {
                std::mem::take(&mut st.refreshes)
            } else {
                Vec::new()
            },
            token: std::mem::take(&mut st.token),
            exec_ns,
            fuel_used,
            imports: st.imports,
            precharge: st.precharge,
            wall_overrun,
            forged: st.forged,
            dropped: st.dropped,
        }
    }

    pub fn store_data_precharge_cap(&mut self, v: u64) {
        self.store.data_mut().precharge_cap = v;
    }
}

fn classify(e: &wasmtime::Error, abort: Option<FaultKind>) -> FaultKind {
    if let Some(k) = abort {
        return k;
    }
    if let Some(t) = e.downcast_ref::<Trap>() {
        return match t {
            Trap::OutOfFuel => FaultKind::OutOfFuel,
            _ => FaultKind::Trap,
        };
    }
    let m = format!("{e:#}");
    if m.contains("too much data is being copied") {
        FaultKind::MalformedEffect
    } else if m.contains("memory") && (m.contains("grow") || m.contains("limit")) {
        FaultKind::LimiterDenied
    } else {
        FaultKind::Trap
    }
}

// ---- the runtime both arms call -----------------------------------------------------------------

/// One W-lane job as the arms see it. Built by the writer; the arms must not change it.
#[derive(Clone, Debug)]
pub struct PluginBatch {
    pub plugin: PluginId,
    pub job: JobId,
    pub slot_epoch: u64,
    pub nodes: Vec<NodeId>,
    pub interactive: bool,
    pub grant: Grant,
    /// Writer clock stamp when the job was created (for `queued`).
    pub created_ns: u64,
}

#[derive(Debug)]
pub struct DoneReply {
    pub plugin: PluginId,
    pub job: JobId,
    pub slot_epoch: u64,
    pub interactive: bool,
    pub grant: Grant,
    pub token: Token,
    pub facts: Vec<FactEffect>,
    pub refreshes: Vec<NodeId>,
    pub exec_ns: u64,
    pub queued_ns: u64,
    pub fuel_used: u64,
    pub wall_overrun: bool,
    pub imports: u32,
    pub dropped: u32,
    pub forged: u32,
}

#[derive(Debug)]
pub struct FaultReply {
    pub plugin: PluginId,
    pub job: JobId,
    pub slot_epoch: u64,
    pub interactive: bool,
    pub grant: Grant,
    pub kind: FaultKind,
    pub exec_ns: u64,
    pub queued_ns: u64,
    pub fuel_used: u64,
}

/// As ADR-0026 section 2.4.6.
#[derive(Debug)]
pub enum PluginReply {
    Done(DoneReply),
    Fault(FaultReply),
    Cancelled {
        plugin: PluginId,
        job: JobId,
        slot_epoch: u64,
    },
}

impl PluginReply {
    pub fn plugin(&self) -> PluginId {
        match self {
            PluginReply::Done(d) => d.plugin,
            PluginReply::Fault(f) => f.plugin,
            PluginReply::Cancelled { plugin, .. } => *plugin,
        }
    }
}

/// A plugin as an executor: owns the `Store`/instance, instantiates lazily (`Cold` -> `Ready`),
/// drops the instance on a fault, and turns a job into a reply. No scheduling policy lives here.
pub struct PluginRuntime {
    pub plugin: PluginId,
    he: Arc<HostEngine>,
    lp: Arc<LoadedPlugin>,
    inst: Option<GuestInstance>,
    mac_key: MacKey,
    pub instantiations: u32,
}

impl PluginRuntime {
    pub fn new(plugin: PluginId, he: Arc<HostEngine>, lp: Arc<LoadedPlugin>) -> PluginRuntime {
        let mac_key = MacKey::random();
        PluginRuntime {
            plugin,
            he,
            lp,
            inst: None,
            mac_key,
            instantiations: 0,
        }
    }

    pub fn name(&self) -> &str {
        &self.lp.name
    }

    pub fn loaded(&self) -> &Arc<LoadedPlugin> {
        &self.lp
    }

    pub fn engine(&self) -> &Arc<HostEngine> {
        &self.he
    }

    /// Instantiate and run `attach` now, off the writer turn (scope attach, background lane).
    pub fn warm(&mut self) -> Result<()> {
        let mut i = GuestInstance::new(&self.lp, self.mac_key.clone())?;
        i.attach(5_000_000)?;
        self.inst = Some(i);
        self.instantiations += 1;
        Ok(())
    }

    pub fn is_ready(&self) -> bool {
        self.inst.is_some()
    }

    /// Drop the instance (the plugin goes `Cold`).
    pub fn drop_instance(&mut self) {
        self.inst = None;
    }

    pub fn run(&mut self, b: &PluginBatch, rows: Arc<FrozenRows>) -> PluginReply {
        let start = now_ns();
        let queued_ns = start.saturating_sub(b.created_ns);
        if self.inst.is_none() {
            match GuestInstance::new(&self.lp, self.mac_key.clone()) {
                Ok(i) => {
                    self.inst = Some(i);
                    self.instantiations += 1;
                }
                Err(_) => {
                    return PluginReply::Fault(FaultReply {
                        plugin: b.plugin,
                        job: b.job,
                        slot_epoch: b.slot_epoch,
                        interactive: b.interactive,
                        grant: b.grant,
                        kind: FaultKind::InstantiateFailed,
                        exec_ns: now_ns() - start,
                        queued_ns,
                        fuel_used: 0,
                    })
                }
            }
        }
        let Some(inst) = self.inst.as_mut() else {
            return PluginReply::Cancelled {
                plugin: b.plugin,
                job: b.job,
                slot_epoch: b.slot_epoch,
            };
        };
        let spec = CallSpec {
            rows,
            nodes: &b.nodes,
            interactive: b.interactive,
            grant: b.grant,
            epoch: b.slot_epoch,
        };
        let r = inst.derive(&spec);
        match r.outcome {
            Ok(()) => PluginReply::Done(DoneReply {
                plugin: b.plugin,
                job: b.job,
                slot_epoch: b.slot_epoch,
                interactive: b.interactive,
                grant: b.grant,
                token: r.token,
                facts: r.facts,
                refreshes: r.refreshes,
                exec_ns: r.exec_ns,
                queued_ns,
                fuel_used: r.fuel_used,
                wall_overrun: r.wall_overrun,
                imports: r.imports,
                dropped: r.dropped,
                forged: r.forged,
            }),
            Err(kind) => {
                self.inst = None;
                PluginReply::Fault(FaultReply {
                    plugin: b.plugin,
                    job: b.job,
                    slot_epoch: b.slot_epoch,
                    interactive: b.interactive,
                    grant: b.grant,
                    kind,
                    exec_ns: r.exec_ns,
                    queued_ns,
                    fuel_used: r.fuel_used,
                })
            }
        }
    }
}

/// Calibrate fuel per ns on this core with the spin guest (`a6-full-grant`): WQ-2 table input.
pub fn calibrate_kappa(he: &Arc<HostEngine>, spin_wasm: &Path, rounds: u32) -> Result<Vec<f64>> {
    let lp = Arc::new(he.load("calib", spin_wasm)?);
    let mut rt = PluginRuntime::new(0, he.clone(), lp);
    let nodes = [2u32];
    let table = crate::model::NodeTable::build(16, 1);
    let rows = crate::model::FrozenRowsFactory { table: &table }.build(&nodes);
    let mut out = Vec::new();
    for _ in 0..rounds {
        let b = PluginBatch {
            plugin: 0,
            job: 0,
            slot_epoch: 1,
            nodes: nodes.to_vec(),
            interactive: true,
            grant: Grant {
                fuel: 2_000_000,
                ns: 10_000_000,
                capped: false,
            },
            created_ns: now_ns(),
        };
        match rt.run(&b, rows.clone()) {
            PluginReply::Fault(f) if f.kind == FaultKind::OutOfFuel => {
                out.push(f.fuel_used as f64 / f.exec_ns.max(1) as f64);
            }
            other => return Err(format_err!("calibration expected OutOfFuel, got {other:?}")),
        }
    }
    Ok(out)
}
