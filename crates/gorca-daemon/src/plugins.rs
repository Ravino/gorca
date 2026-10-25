// SPDX-License-Identifier: GPL-3.0-or-later
//! The plugin runtime as the core's fusion writers see it (ADR-0026 M18a): loads the packages,
//! matches them to scopes and runs their `derive` on plugin actors behind the core's
//! [`PluginService`] / [`ScopeRunner`] traits. The only place that joins `gorca-core` and
//! `gorca-plugin-host`.
//!
//! * Packages come from the first-party directory (tier T2 if pinned, otherwise refused) and
//!   `$XDG_DATA_HOME/gorca/plugins` (third party, untrusted until the user marks it).
//! * A scope gets one actor per matching plugin. Every plugin has a writer-owned [`Slot`]
//!   (strikes, backoff, disable) whose events are the replies of the actor.
//! * A busy plugin is never skipped: its newest job is sent anyway and displaces the queued one
//!   (the displaced job's nodes are re-queued by the core), so one slow plugin cannot starve.
//! * An unverified application identity gets first-party, non-URL-scoped plugins only.
//! * Scheduling of the interactive handoff is the statistical scheduler SPS (ADR-0026 section
//!   2.4.7, crate `gorca-plugin-sched`) or, behind `[plugins] scheduler = "labels"`, the fixed
//!   interim policy. Under SPS the key-down hub ([`Prewake`]) pre-wakes a few plugin workers per
//!   key under a CPU token bucket, every focus change gets a plan (placement, fuel share, hold
//!   per plugin) and the writer waits for each waited plugin up to its own hold, never past the
//!   absolute clamp `t0 + D - fuse reserve`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use gorca_core::plugin_port::{
    Dirty, FactEffect, FactValue, Governor, Handoff, Job, PluginMeta, PluginService, PluginTier,
    Reply, ReplyBody, Row, ScopeRunner, ScopeSpec,
};
use gorca_plugin_host::actor::{ActorRunner, Config as ActorConfig};
use gorca_plugin_host::cache::CwasmCache;
use gorca_plugin_host::call::{
    DeriveJob, Exec, PlannedJob, PluginExec, PluginReply, ReplySink, ReplyTiming, Turn, WLaneRunner,
};
use gorca_plugin_host::host::{Effect, Target, Value};
use gorca_plugin_host::instance::{DirtyNode, EngineOptions, Loaded, build_engine};
use gorca_plugin_host::limits::{B1_BACKGROUND, B2_INTERACTIVE, FuelGrant, KAPPA_PROVISIONAL};
use gorca_plugin_host::package::{
    Manifest, Package, Pin, Priority, Source, Watch, World, load_dir,
};
use gorca_plugin_host::slot::{Action, Event, JobId, Slot, SlotState};
use gorca_plugin_host::trust::{Perms, SystemScope, TrustTier};
use gorca_plugin_host::urlmatch::UrlPattern;
use gorca_plugin_host::view::{ModelView, ScopeInfo};
use gorca_plugin_sched::{
    Assignment, KeyClass, Label, Observation, Params as SchedParams, PlaceMode, Scheduler,
    SpinBucket, Start, Tier, Triggered, TurnTimes,
};

/// The bounded wait window `D` (ADR-0026 r4).
const D_WINDOW: Duration = Duration::from_micros(300);

/// How long a key-down keeps the idle plugin workers spinning under the `labels` policy: the
/// focus change that follows a key arrives within a few milliseconds, and a running worker
/// answers it without a wake-up.
const PREWAKE_SPIN: Duration = Duration::from_millis(8);
/// Under `labels`, pre-wakes at most this often (a typist's repeat rate must not keep every core
/// hot).
const PREWAKE_EVERY_NS: u64 = 20_000_000;
/// Reserved inside `D` for fusing after the wait, on top of admitting the replies (ADR section
/// 2.4.7.3: default 8 us).
const FUSE_RESERVE: Duration = Duration::from_micros(24);
/// Prior for the cost unit of admitting replies after the wait, until a turn has measured it.
/// Admitting `n` replies costs about `unit * (n + 1)`: a fixed part about as large as one reply
/// (measured: 1 reply 12 us, 10 replies 60-90 us).
const ADMIT_UNIT_PRIOR_NS: u64 = 5_000;
/// The cost unit never exceeds this, whatever the recent peak says.
const UNIT_CAP_NS: u64 = 12_000;

/// Which policy schedules the interactive handoff (`[plugins] scheduler`, ADR-0026 section
/// 2.4.7.12, C-4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SchedulerPolicy {
    /// The statistical scheduler. Opt-in: C-4 has not passed on a quiet host yet (see
    /// `docs/reports/m18a-3-sps.md`).
    Sps,
    /// The fixed interim policy: pre-wake every worker on every key (rate-limited), wait for
    /// every non-refinement plugin up to `D`, serial-equivalent fuel shares.
    #[default]
    Labels,
}

impl SchedulerPolicy {
    /// `"sps"` or `"labels"`.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "sps" => Some(Self::Sps),
            "labels" => Some(Self::Labels),
            _ => None,
        }
    }

    /// The name used in the settings file.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Sps => "sps",
            Self::Labels => "labels",
        }
    }
}

/// Hook point for the host idle-state decision DP-1 (ADR-0026 section 2.4.7.10, **pending the
/// owner**). **No implementation ships and nothing is installed**: `Prewake` has no hook unless a
/// caller sets one, so the behaviour is DP-1 option (a), accept the host's idle states.
///
/// If the owner picks option (b), a `/dev/cpu_dma_latency` lease implements this trait: take (or
/// renew) the lease in [`IdleStateHook::plugins_active`], release it 2 s after the last call and
/// force a re-take after 30 s of continuous holding (section 2.4.7.10, "Definition of plugins
/// active"). A failure to open the node must be logged once (`pm-qos-unavailable`, errno) and must
/// not change anything else. No udev rule or other system change belongs in this crate.
pub trait IdleStateHook: Send + Sync + std::fmt::Debug {
    /// Plugins became active: a key-down armed at least one worker. Called from the keyboard
    /// pump, so it must not block (an atomic store and at most one non-blocking syscall).
    fn plugins_active(&self, now_ns: u64);
}

/// The SPS state of one scope: written by the scope's writer (plan, observe) and by the key-down
/// hub (pre-wake). Both hold the lock for microseconds; the hub only `try_lock`s.
#[derive(Debug)]
struct Sps {
    sched: Mutex<Scheduler>,
}

/// One registered scope.
#[derive(Debug)]
struct Hook {
    handle: gorca_plugin_host::actor::PrewakeHandle,
    sps: Option<Arc<Sps>>,
}

/// The key-down hook of the plugin runtime: the keyboard pump calls [`Prewake::key_down_class`]
/// before the key reaches the Reader, so the workers that the focus change will need are already
/// running when the writer turn starts its bounded wait. Never blocks.
///
/// Under SPS each scope's scheduler decides how many workers and which to arm, and all scopes
/// draw on one CPU token bucket (SPS-9: at most 200 ms of burst and 150 ms of spin per second,
/// per process). Under `labels` every worker of every scope is armed, at most once per 20 ms.
#[derive(Debug)]
pub struct Prewake {
    scopes: Mutex<Vec<Hook>>,
    last_ns: std::sync::atomic::AtomicU64,
    bucket: Mutex<SpinBucket>,
    /// Key-downs that found a lock busy and did nothing (diagnostics).
    contended: std::sync::atomic::AtomicU64,
    /// The DP-1 hook point; unset (the default) means no lease and no system change.
    idle_hook: OnceLock<Arc<dyn IdleStateHook>>,
}

impl Default for Prewake {
    fn default() -> Self {
        Self {
            scopes: Mutex::new(Vec::new()),
            last_ns: std::sync::atomic::AtomicU64::new(0),
            bucket: Mutex::new(SpinBucket::standard()),
            contended: std::sync::atomic::AtomicU64::new(0),
            idle_hook: OnceLock::new(),
        }
    }
}

impl Prewake {
    /// Install the DP-1 hook (once; a second call is ignored). Default off: nothing calls this in
    /// the product until the owner decides DP-1.
    pub fn set_idle_hook(&self, hook: Arc<dyn IdleStateHook>) -> bool {
        self.idle_hook.set(hook).is_ok()
    }

    fn register(&self, handle: gorca_plugin_host::actor::PrewakeHandle, sps: Option<Arc<Sps>>) {
        lock(&self.scopes).push(Hook { handle, sps });
    }

    /// A key went down (class unknown: treated as navigation). A contended or recent call does
    /// nothing.
    pub fn key_down(&self) {
        self.key_down_class(KeyClass::Nav);
    }

    /// A key event of the keyboard pump: only presses of keys that can change the focus count;
    /// modifiers alone and releases do nothing.
    pub fn key_event(&self, ev: &gorca_input_api::KeyEvent) {
        if let Some(class) = key_class(ev) {
            self.key_down_class(class);
        }
    }

    /// A key of `class` went down.
    pub fn key_down_class(&self, class: KeyClass) {
        use std::sync::atomic::Ordering::{AcqRel, Acquire, Relaxed};
        let now = gorca_rt::process::monotonic_ns();
        let Ok(mut hooks) = self.scopes.try_lock() else {
            self.contended.fetch_add(1, Relaxed);
            return;
        };
        // Under `labels` a recent pre-wake is enough; the SPS scopes decide for themselves.
        let last = self.last_ns.load(Acquire);
        let labels_due = now.saturating_sub(last) >= PREWAKE_EVERY_NS
            && self
                .last_ns
                .compare_exchange(last, now, AcqRel, Acquire)
                .is_ok();
        let mut bucket = self.bucket.try_lock().ok();
        let mut armed_any = false;
        hooks.retain(|h| match &h.sps {
            None => {
                armed_any |= labels_due;
                if labels_due {
                    h.handle.arm(PREWAKE_SPIN)
                } else {
                    h.handle.arm_set(0, 0)
                }
            }
            Some(sps) => {
                let (Ok(mut sched), Some(bucket)) = (sps.sched.try_lock(), bucket.as_mut()) else {
                    self.contended.fetch_add(1, Relaxed);
                    return h.handle.arm_set(0, 0);
                };
                let pw = sched.on_key_down(class, now / 1000, bucket);
                armed_any |= pw.threads > 0;
                h.handle
                    .arm_set(u64::from(pw.who), pw.until_us.saturating_mul(1000))
            }
        });
        if armed_any && let Some(hook) = self.idle_hook.get() {
            hook.plugins_active(now);
        }
    }

    /// Key-downs that did nothing because a lock was busy (diagnostics).
    pub fn contended(&self) -> u64 {
        self.contended.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Scopes registered and still alive (tests).
    pub fn len(&self) -> usize {
        lock(&self.scopes).len()
    }

    /// True when no scope is registered.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The class of a key event for the scheduler, `None` for what cannot change the focus (releases,
/// modifier keys alone).
#[must_use]
pub fn key_class(ev: &gorca_input_api::KeyEvent) -> Option<KeyClass> {
    use gorca_input_api::Modifiers;
    if !ev.pressed {
        return None;
    }
    match ev.keysym {
        // Shift_L .. Hyper_R, Num_Lock, ISO_Level3_Shift: modifiers and locks.
        0xffe1..=0xffee | 0xff7f | 0xfe03 => None,
        0xff09 | 0xfe20 => Some(KeyClass::Tab),
        0xff0d | 0xff8d => Some(KeyClass::Enter),
        0xff50..=0xff57 | 0xff96..=0xff9c => Some(KeyClass::Nav),
        _ => {
            let chord = ev
                .modifiers
                .intersects(Modifiers::CONTROL | Modifiers::MOD1 | Modifiers::MOD4);
            Some(if ev.unichar != 0 && !chord {
                KeyClass::Type
            } else {
                KeyClass::Other
            })
        }
    }
}

/// First-party pins: `<id> <sha256 hex>` per line, generated by `tools/build-plugins.sh`.
const PINS_TEXT: &str = include_str!("../../../plugins/pins.txt");

/// The pin list compiled into the daemon.
fn first_party_pins() -> &'static [Pin] {
    static PINS: OnceLock<Vec<Pin>> = OnceLock::new();
    PINS.get_or_init(|| parse_pins(PINS_TEXT))
}

/// Parse a pin list (`<id> <64 hex digits>`, `#` comments). Bad lines are skipped: an unpinned
/// package is refused, which fails closed.
pub fn parse_pins(text: &str) -> Vec<Pin> {
    text.lines()
        .filter_map(|l| {
            let l = l.trim();
            if l.is_empty() || l.starts_with('#') {
                return None;
            }
            let (id, hex) = l.split_once(char::is_whitespace)?;
            let hex = hex.trim();
            if hex.len() != 64 {
                return None;
            }
            let mut sha = [0u8; 32];
            for (i, b) in sha.iter_mut().enumerate() {
                *b = u8::from_str_radix(hex.get(2 * i..2 * i + 2)?, 16).ok()?;
            }
            // The list lives as long as the daemon.
            let id: &'static str = Box::leak(id.to_owned().into_boxed_str());
            Some(Pin { id, sha256: sha })
        })
        .collect()
}

/// Where the runtime looks and what it adds for tests.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PluginOptions {
    /// `$XDG_DATA_HOME/gorca/plugins`.
    pub user_dir: Option<PathBuf>,
    /// The first-party directory (`/usr/lib/gorca/addons`).
    pub first_party_dir: Option<PathBuf>,
    /// The compiled-component cache directory.
    pub cache_dir: Option<PathBuf>,
    /// `id=pattern` pairs adding a URL pattern (`file://` allowed) to a loaded plugin: the
    /// private-session test knob, never read from a config file.
    pub url_extra: Vec<(String, String)>,
    /// Compile for the portable baseline (`GORCA_CPU_DISABLE=all`).
    pub portable: bool,
    /// Load third-party packages from `user_dir`. **Refused unless set, and only tests set it**
    /// (never from the environment or a file): until M18a-3 ships `system-scopes`, per-plugin
    /// enable and the spoken warning, T3 code must not run (ADR-0026 section 2.7, fail closed).
    pub allow_sideloaded: bool,
    /// How long the writer spins in its bounded wait before it parks. `None`: the whole window
    /// (`D`, 0.3 ms): a parked writer wakes late from a deep idle state, a spinning one does not.
    /// Costs at most 0.3 ms of one core per focus change.
    pub writer_spin: Option<Duration>,
    /// `[plugins] scheduler`: the scheduling policy of the interactive handoff.
    pub scheduler: SchedulerPolicy,
    /// Most workers SPS pre-wakes per key (`None`: the ADR default of 6). A diagnostic knob of the
    /// benchmarks, never read from a file.
    pub sps_k_max: Option<usize>,
    /// `GORCA_SCHED_LOG`: append the SPS input log of every scope to this file when the scope
    /// closes (numbers only, ADR-0026 SPS-6). Set by `main` from the environment.
    pub sched_log: Option<PathBuf>,
}

struct Entry {
    manifest: Manifest,
    tier: TrustTier,
    grants: Perms,
    loaded: Loaded,
    extra_urls: Vec<UrlPattern>,
}

impl Entry {
    /// The executor of one actor: the plugin instance, with its message facts resolved on the
    /// worker thread (the writer's admission stays cheap).
    fn exec(&self) -> Box<dyn Exec> {
        Box::new(ResolvingExec {
            inner: PluginExec::new(
                self.loaded.clone(),
                self.grants,
                self.tier,
                self.manifest.memory_mib,
            ),
            ns: self.manifest.id.clone(),
        })
    }

    fn url_scoped(&self) -> bool {
        !self.manifest.urls.is_empty() || !self.extra_urls.is_empty()
    }

    fn matches(&self, scope: &ScopeSpec) -> bool {
        // K2 plugins serve one document, K1 plugins one application.
        match (self.manifest.world, scope.doc) {
            (World::Site, true) | (World::AppModule, false) => {}
            (World::Site, false) | (World::AppModule, true) => return false,
        }
        let url = scope.url.as_deref();
        let app_ok =
            self.manifest.apps.is_empty() || self.manifest.apps.iter().any(|a| **a == *scope.app);
        let url_ok = !self.url_scoped()
            || url.is_some_and(|u| {
                self.manifest.urls.iter().any(|p| p.matches(u))
                    || self.extra_urls.iter().any(|p| p.matches(u))
            });
        // An unverified identity gets first-party plugins without a URL scope only: the
        // application's name is what it says about itself (ADR-0026 section 2.7).
        let identity_ok =
            scope.verified || (self.tier == TrustTier::FirstParty && !self.url_scoped());
        app_ok && url_ok && identity_ok
    }
}

/// The plugin runtime: the loaded packages and the shared keypress-path governor.
pub struct PluginRuntime {
    entries: Vec<Arc<Entry>>,
    gov: Arc<Governor>,
    wants_url: bool,
    prewake: Arc<Prewake>,
    writer_spin: Duration,
    policy: SchedulerPolicy,
    sps_k_max: Option<usize>,
    sched_log: Option<PathBuf>,
}

impl std::fmt::Debug for PluginRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginRuntime")
            .field(
                "plugins",
                &self
                    .entries
                    .iter()
                    .map(|e| e.manifest.id.as_str())
                    .collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

/// The plugin's message catalogs (`i18n/en.msg`, `i18n/ru.msg`, the format of the core's catalogs)
/// under its id as the namespace: the texts its message facts resolve to. Bounded reads; a missing
/// or refused file only means those messages are missing.
fn register_catalogs(id: &str, dir: &Path) {
    use std::io::Read as _;
    const CAP: u64 = 256 * 1024;
    let mut files: Vec<(gorca_i18n::Lang, String)> = Vec::new();
    for (lang, name) in [
        (gorca_i18n::Lang::En, "en.msg"),
        (gorca_i18n::Lang::Ru, "ru.msg"),
    ] {
        let Ok(f) = gorca_plugin_host::fsx::open_regular(&dir.join("i18n").join(name)) else {
            continue;
        };
        let mut text = String::new();
        if f.take(CAP).read_to_string(&mut text).is_ok() {
            files.push((lang, text));
        }
    }
    if files.is_empty() {
        return;
    }
    let refs: Vec<(gorca_i18n::Lang, &str)> = files.iter().map(|(l, t)| (*l, t.as_str())).collect();
    match gorca_i18n::ext::register_extension(id, &refs) {
        Ok(errs) if !errs.is_empty() => {
            tracing::warn!(
                id,
                lines = errs.len(),
                "plugin catalog has lines that do not parse"
            );
        }
        Ok(_) => {}
        Err(e) => tracing::warn!(id, error = %e, "plugin catalog refused"),
    }
}

fn package_dirs(root: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut v: Vec<PathBuf> = rd
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.join("manifest.toml").is_file())
        .collect();
    v.sort();
    v
}

impl PluginRuntime {
    /// Load every package. Blocking (compiles components on a cache miss): call it off the
    /// async executor. A package that fails to load is logged and skipped; nothing here can
    /// stop the daemon.
    pub fn load(opts: &PluginOptions) -> Arc<PluginRuntime> {
        Self::load_with_pins(opts, first_party_pins())
    }

    /// [`PluginRuntime::load`] registering every scope with `prewake` (the daemon's key hook).
    pub fn load_prewake(opts: &PluginOptions, prewake: Arc<Prewake>) -> Arc<PluginRuntime> {
        Self::load_full(opts, first_party_pins(), prewake)
    }

    /// [`PluginRuntime::load`] with an explicit pin list (tests).
    pub fn load_with_pins(opts: &PluginOptions, pins: &[Pin]) -> Arc<PluginRuntime> {
        Self::load_full(opts, pins, Arc::new(Prewake::default()))
    }

    fn load_full(opts: &PluginOptions, pins: &[Pin], prewake: Arc<Prewake>) -> Arc<PluginRuntime> {
        let mut entries: Vec<Arc<Entry>> = Vec::new();
        let engine = match build_engine(EngineOptions {
            portable: opts.portable,
        }) {
            Ok(e) => e,
            Err(e) => {
                tracing::error!(error = %e, "plugin engine does not build: plugins are off");
                return Arc::new(Self::empty());
            }
        };
        let cache = opts
            .cache_dir
            .as_deref()
            .and_then(|d| match CwasmCache::open(d) {
                Ok(c) => Some(c),
                Err(e) => {
                    tracing::warn!(error = %e, "plugin cache unavailable: compiling every start");
                    None
                }
            });
        let mut sources: Vec<(PathBuf, Source)> = Vec::new();
        if let Some(d) = &opts.first_party_dir {
            sources.extend(package_dirs(d).into_iter().map(|p| (p, Source::Installed)));
        }
        if let Some(d) = &opts.user_dir {
            let found = package_dirs(d);
            if opts.allow_sideloaded {
                sources.extend(found.into_iter().map(|p| (p, Source::Sideloaded)));
            } else if !found.is_empty() {
                // Fail closed: no system-scope classification exists yet (M18a-3), so no
                // third-party code runs. First-party packages are unaffected.
                tracing::warn!(
                    dir = %d.display(),
                    packages = found.len(),
                    "third-party plugin packages are refused until system-scopes ships (ADR-0026 M18a-3); first-party plugins keep working"
                );
            }
        }
        for (dir, source) in sources {
            let pkg: Package = match load_dir(&dir, source, false, Perms::empty(), pins) {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!(dir = %dir.display(), error = %e, "plugin package refused");
                    continue;
                }
            };
            if entries.iter().any(|e| e.manifest.id == pkg.manifest.id) {
                tracing::warn!(id = %pkg.manifest.id, "duplicate plugin id: the first one wins");
                continue;
            }
            let loaded = match Loaded::from_package(&engine, cache.as_ref(), &pkg.wasm, &pkg.sha256)
            {
                Ok(l) => l,
                Err(e) => {
                    tracing::warn!(id = %pkg.manifest.id, error = %e, "plugin does not compile or link");
                    continue;
                }
            };
            let extra_urls = opts
                .url_extra
                .iter()
                .filter(|(id, _)| *id == pkg.manifest.id)
                .filter_map(|(_, p)| match UrlPattern::parse_dev(p) {
                    Ok(u) => Some(u),
                    Err(e) => {
                        tracing::warn!(pattern = %p, error = %e, "extra URL pattern refused");
                        None
                    }
                })
                .collect();
            register_catalogs(&pkg.manifest.id, &dir);
            tracing::info!(id = %pkg.manifest.id, tier = ?pkg.tier, "plugin loaded");
            entries.push(Arc::new(Entry {
                manifest: pkg.manifest,
                tier: pkg.tier,
                grants: pkg.grants,
                loaded,
                extra_urls,
            }));
        }
        let wants_url = entries.iter().any(|e| e.url_scoped());
        Arc::new(PluginRuntime {
            entries,
            gov: Arc::new(Governor::default()),
            wants_url,
            prewake,
            writer_spin: opts.writer_spin.unwrap_or(D_WINDOW),
            policy: opts.scheduler,
            sps_k_max: opts.sps_k_max,
            sched_log: opts.sched_log.clone(),
        })
    }

    /// A runtime with no plugins.
    pub fn empty() -> Self {
        Self {
            entries: Vec::new(),
            gov: Arc::new(Governor::default()),
            wants_url: false,
            prewake: Arc::new(Prewake::default()),
            writer_spin: D_WINDOW,
            policy: SchedulerPolicy::default(),
            sps_k_max: None,
            sched_log: None,
        }
    }

    /// The key-down hub the scopes register with.
    pub fn prewake(&self) -> Arc<Prewake> {
        Arc::clone(&self.prewake)
    }

    /// Loaded plugin ids (diagnostics).
    pub fn ids(&self) -> Vec<String> {
        self.entries.iter().map(|e| e.manifest.id.clone()).collect()
    }
}

/// Entries of the `GORCA_SCHED_LOG` ring of one scope.
const SCHED_LOG_ENTRIES: usize = 1 << 16;

fn label_of(p: Priority) -> Label {
    match p {
        Priority::Interactive => Label::Interactive,
        Priority::Unlabelled => Label::Unlabelled,
        Priority::Refinement => Label::Refinement,
    }
}

fn tier_of(t: TrustTier) -> Tier {
    match t {
        TrustTier::FirstParty => Tier::First,
        TrustTier::TrustedThirdParty => Tier::Trusted,
        TrustTier::Untrusted | TrustTier::Dev => Tier::Third,
    }
}

fn core_tier(t: TrustTier) -> PluginTier {
    match t {
        TrustTier::FirstParty => PluginTier::FirstParty,
        TrustTier::TrustedThirdParty => PluginTier::TrustedThirdParty,
        TrustTier::Untrusted | TrustTier::Dev => PluginTier::Untrusted,
    }
}

impl PluginService for PluginRuntime {
    fn wants_url(&self) -> bool {
        self.wants_url
    }

    fn open(
        &self,
        scope: &ScopeSpec,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Option<Box<dyn ScopeRunner>> {
        let matched: Vec<Arc<Entry>> = self
            .entries
            .iter()
            .filter(|e| e.matches(scope))
            .cloned()
            .collect();
        if matched.is_empty() {
            return None;
        }
        let execs: Vec<(u32, Box<dyn Exec>)> = matched
            .iter()
            .enumerate()
            .map(|(i, e)| (u32::try_from(i).unwrap_or(0), e.exec()))
            .collect();
        let for_maker = matched.clone();
        let maker: Box<dyn Fn(u32) -> Option<Box<dyn Exec>> + Send + Sync> = Box::new(move |id| {
            let e = for_maker.get(usize::try_from(id).ok()?)?;
            Some(e.exec())
        });
        let cfg = ActorConfig {
            spin: self.writer_spin,
            ..ActorConfig::default()
        };
        let runner = match ActorRunner::new(execs, maker, cfg) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "plugin actors did not start");
                return None;
            }
        };
        let plugins: Vec<ScopePlugin> = matched
            .iter()
            .map(|e| {
                ScopePlugin::new(
                    PluginMeta {
                        name: Arc::from(e.manifest.id.as_str()),
                        tier: core_tier(e.tier),
                    },
                    e.manifest.watches.clone(),
                )
                .with_priority(e.manifest.priority)
            })
            .collect();
        tracing::info!(app = %scope.app, doc = scope.doc, plugins = matched.len(), scheduler = self.policy.name(), "plugin scope opened");
        let sps = (self.policy == SchedulerPolicy::Sps).then(|| {
            let labels: Vec<(Label, Tier)> = matched
                .iter()
                .map(|e| (label_of(e.manifest.priority), tier_of(e.tier)))
                .collect();
            let mut sched = Scheduler::new(
                &labels,
                SchedParams {
                    kappa: i64::try_from(KAPPA_PROVISIONAL).unwrap_or(21_739),
                    k_max: self.sps_k_max.unwrap_or(SchedParams::default().k_max),
                    ..SchedParams::default()
                },
            );
            if self.sched_log.is_some() {
                sched.record(SCHED_LOG_ENTRIES);
            }
            Arc::new(Sps {
                sched: Mutex::new(sched),
            })
        });
        self.prewake.register(runner.prewake_handle(), sps.clone());
        let mut scope = PluginScope::from_parts_sps(runner, plugins, wake, sps);
        scope.sched_log.clone_from(&self.sched_log);
        Some(Box::new(scope))
    }

    fn governor(&self) -> Arc<Governor> {
        Arc::clone(&self.gov)
    }
}

// ---- the reply queue ---------------------------------------------------------------------

struct Sink {
    q: Mutex<Vec<(PluginReply, Option<ReplyTiming>)>>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl ReplySink for Sink {
    fn send(&self, reply: PluginReply) {
        lock(&self.q).push((reply, None));
        (self.wake)();
    }

    fn send_timed(&self, reply: PluginReply, timing: ReplyTiming) {
        lock(&self.q).push((reply, Some(timing)));
        (self.wake)();
    }
}

// ---- the snapshot as a model view --------------------------------------------------------

#[derive(Debug)]
struct RowsView {
    rows: Arc<gorca_core::plugin_port::FrozenRows>,
}

impl ModelView for RowsView {
    fn generation(&self, index: u32) -> Option<u16> {
        self.rows.rows.get(&index).map(|r| r.generation)
    }
    fn role(&self, index: u32) -> Option<u16> {
        self.rows.rows.get(&index).map(|r| r.row.role)
    }
    fn states(&self, index: u32) -> u64 {
        self.rows.rows.get(&index).map_or(0, |r| r.row.states)
    }
    fn name(&self, index: u32) -> Option<String> {
        self.rows
            .rows
            .get(&index)
            .and_then(|r| r.row.name.as_deref().map(str::to_owned))
    }
    fn text(&self, index: u32, start: u32, len: u32) -> Option<String> {
        let t = self.rows.rows.get(&index)?.row.text.as_deref()?;
        let from = usize::try_from(start).ok()?.min(t.len());
        let to = from.saturating_add(usize::try_from(len).ok()?).min(t.len());
        // Byte offsets cut to character boundaries.
        let from = (from..=t.len()).find(|i| t.is_char_boundary(*i))?;
        let to = (to..=t.len()).find(|i| t.is_char_boundary(*i))?;
        Some(t[from..to].to_owned())
    }
    fn attr(&self, index: u32, key: &str) -> Option<String> {
        self.rows
            .rows
            .get(&index)
            .and_then(|r| r.row.attr(key).map(str::to_owned))
    }
    fn parent(&self, index: u32) -> Option<u32> {
        self.rows.rows.get(&index).and_then(|r| r.parent)
    }
    fn children(&self, index: u32, max: u16) -> Vec<u32> {
        self.rows.rows.get(&index).map_or_else(Vec::new, |r| {
            r.children
                .iter()
                .copied()
                .filter(|c| self.rows.rows.contains_key(c))
                .take(usize::from(max))
                .collect()
        })
    }
    fn scope(&self) -> ScopeInfo {
        let s = &self.rows.scope;
        ScopeInfo {
            name: s.app.to_string(),
            engine: s.engine,
            verified: s.verified,
            url: s.url.as_deref().map(str::to_owned),
        }
    }
    fn system_scope(&self, index: u32) -> Option<SystemScope> {
        self.rows
            .rows
            .get(&index)
            .and_then(|r| r.row.secret.then_some(SystemScope::SecretNode))
    }
}

// ---- one scope ---------------------------------------------------------------------------

struct ScopePlugin {
    /// Refinement plugins never gate a turn (their jobs go to the background slot).
    priority: Priority,
    meta: PluginMeta,
    watches: Vec<Watch>,
    slot: Slot,
    masked: bool,
    /// Moving average of the guest wall time of a call, seconds (prediction for the split).
    exec_ewma: f64,
}

impl ScopePlugin {
    fn with_priority(mut self, p: Priority) -> Self {
        self.priority = p;
        self
    }

    fn new(meta: PluginMeta, watches: Vec<Watch>) -> Self {
        let mut slot = Slot::new();
        // The components are pre-loaded (the actors built their instances).
        let _ = slot.event(Event::PreloadDone, Instant::now());
        Self {
            priority: Priority::Interactive,
            meta,
            watches,
            slot,
            masked: false,
            exec_ewma: B2_INTERACTIVE.as_secs_f64() / 4.0,
        }
    }
}

/// What the scheduler decided for a job, kept until its reply is observed.
#[derive(Clone, Copy)]
struct PlanInfo {
    a: Assignment,
    waited: bool,
    /// `CLOCK_MONOTONIC` ns at which the plan was made (the send follows within tens of
    /// microseconds; diagnostics only, the observation uses the actor's own hand-off stamp).
    sent_ns: u64,
    /// Trace: turn start, the absolute clamp of the wait, the reserve, the turn number.
    t0_ns: u64,
    clamp_ns: u64,
    reserve_us: u32,
    spent_us: u32,
    turn_seq: u64,
}

struct JobInfo {
    /// The SPS decision for an interactive-class job.
    plan: Option<PlanInfo>,
    /// Sent as an interactive job, with this much time.
    interactive: bool,
    grant: Duration,
    plugin: usize,
    t: u64,
    targets: Vec<u32>,
    slot_epoch: u64,
}

/// The scope's plugins behind the core's runner trait.
struct PluginScope {
    runner: ActorRunner,
    metas: Vec<PluginMeta>,
    plugins: Vec<ScopePlugin>,
    sink: Arc<Sink>,
    next_job: JobId,
    jobs: HashMap<JobId, JobInfo>,
    /// Rotating offset of the progressive-filling ties (MQ-3).
    rotate: usize,
    /// Replies decided locally (a skipped slot), delivered by the next poll.
    synthetic: Vec<Reply>,
    /// Nanoseconds added to the clock the slots see (tests move a backoff to its end without
    /// waiting).
    skew: Arc<std::sync::atomic::AtomicU64>,
    /// The statistical scheduler of the scope (`None` under the `labels` policy).
    sps: Option<Arc<Sps>>,
    /// Where the scheduler's input log goes when the scope closes.
    sched_log: Option<PathBuf>,
    /// Trace only: the number of the SPS turn and when its wait ended.
    turn_seq: u64,
    last_end_ns: u64,
    /// Cost unit of admitting replies after the wait, ns: admitting `n` costs about
    /// `unit * (n + 1)`. Tracks the recent peak (the reserve of the wait covers the tail).
    admit_unit_ns: u64,
}

/// The fuel split of an interactive handoff (ADR-0026 section 2.4.2, progressive filling):
/// the pool is `D` minus the entry cost of every job; jobs are served by predicted time
/// ascending (ties broken by the rotating offset) and each gets `min(cap, remaining / left)`;
/// the predicted use returns the rest to the pool.
pub fn progressive_fill(
    pool: Duration,
    cap: Duration,
    predicted: &[Duration],
    rotate: usize,
) -> Vec<Duration> {
    let n = predicted.len();
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by_key(|i| (predicted[*i], (i + n - rotate % n.max(1)) % n.max(1)));
    let mut grants = vec![Duration::ZERO; n];
    let mut remaining = pool;
    for (k, i) in order.into_iter().enumerate() {
        let left = u32::try_from(n - k).unwrap_or(1).max(1);
        let a = cap.min(remaining / left);
        grants[i] = a;
        remaining = remaining.saturating_sub(a.min(predicted[i]));
    }
    grants
}

/// Cost of marshalling one job outside the guest fuel (c_max, ADR-0026 section 2.4.2).
const C_MAX: Duration = Duration::from_micros(2);

impl PluginScope {
    #[cfg(test)]
    fn from_parts(
        runner: ActorRunner,
        plugins: Vec<ScopePlugin>,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        Self::from_parts_sps(runner, plugins, wake, None)
    }

    fn from_parts_sps(
        runner: ActorRunner,
        plugins: Vec<ScopePlugin>,
        wake: Arc<dyn Fn() + Send + Sync>,
        sps: Option<Arc<Sps>>,
    ) -> Self {
        let metas = plugins.iter().map(|p| p.meta.clone()).collect();
        Self {
            runner,
            metas,
            plugins,
            sink: Arc::new(Sink {
                q: Mutex::new(Vec::new()),
                wake,
            }),
            next_job: 1,
            jobs: HashMap::new(),
            rotate: 0,
            synthetic: Vec::new(),
            skew: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            sps,
            sched_log: None,
            turn_seq: 0,
            last_end_ns: 0,
            admit_unit_ns: ADMIT_UNIT_PRIOR_NS,
        }
    }

    fn now(&self) -> Instant {
        Instant::now() + Duration::from_nanos(self.skew.load(std::sync::atomic::Ordering::Relaxed))
    }

    fn new_job(
        &mut self,
        plugin: usize,
        job: &Job,
        interactive: bool,
        grant: FuelGrant,
    ) -> Option<DeriveJob> {
        let now = self.now();
        let p = self.plugins.get_mut(plugin)?;
        let id = self.next_job;
        self.next_job += 1;
        let targets: Vec<u32> = job.nodes.iter().map(|d| d.node).collect();
        // A demoted plugin is called as background class only.
        let interactive = interactive && p.slot.interactive_allowed(now);
        match p.slot.event(Event::CallNeeded(id), now) {
            // A busy slot is not a reason to skip: the newest job goes to the actor, displaces
            // the queued one (whose nodes the core re-queues) and runs next.
            Action::Send | Action::Coalesce => {}
            Action::Skip
            | Action::Nothing
            | Action::Preload
            | Action::DropInstance
            | Action::DropInstanceAndNotice
            | Action::DropSlot
            | Action::Impossible => {
                self.synthetic.push(Reply {
                    plugin,
                    t: job.t,
                    targets,
                    body: ReplyBody::Failed { requeue: false },
                });
                return None;
            }
        }
        let slot_epoch = p.slot.slot_epoch();
        self.jobs.insert(
            id,
            JobInfo {
                plan: None,
                interactive,
                grant: grant.time,
                plugin,
                t: job.t,
                targets,
                slot_epoch,
            },
        );
        let nodes = job
            .nodes
            .iter()
            .map(|d: &Dirty| DirtyNode {
                index: d.node,
                watch: d.watch,
                cause: d.cause,
            })
            .collect();
        let view: Arc<dyn ModelView> = Arc::new(RowsView {
            rows: Arc::clone(&job.rows),
        });
        Some(DeriveJob {
            plugin: u32::try_from(plugin).unwrap_or(0),
            job: id,
            slot_epoch,
            interactive,
            epoch: job.t,
            nodes,
            view,
            grant,
            queued_at: Instant::now(),
        })
    }

    /// The SPS handoff (ADR-0026 section 2.4.7.4): one `plan` for the triggered set, every job
    /// dispatched in the same turn in plan order, a hold per waited plugin, and the absolute clamp
    /// `t0 + D - fuse reserve` on the wait.
    fn interactive_sps(
        &mut self,
        sps: &Arc<Sps>,
        t0_ns: u64,
        wait: bool,
        jobs: Vec<Job>,
    ) -> Handoff {
        let now = gorca_core::plugin_port::now_ns();
        let spent_us = u32::try_from(now.saturating_sub(t0_ns) / 1000).unwrap_or(u32::MAX);
        // A plugin still busy with an earlier job cannot answer inside this wait; a `refinement`
        // plugin never gates a turn. Their jobs are dispatched in this turn as background class
        // (they run next, newest wins) and stay out of the wait.
        let trig: Vec<Triggered> = jobs
            .iter()
            .map(|j| Triggered {
                pid: j.plugin,
                busy: self.jobs.values().any(|i| i.plugin == j.plugin),
            })
            .collect();
        let mask = trig
            .iter()
            .filter(|t| t.pid < 32)
            .fold(0u32, |m, t| m | (1 << t.pid));
        // What follows the wait inside the same turn: fusing, and admitting every reply that is
        // in by then (`unit * (n + 1)`). The planned window assumes the best case, one reply; the
        // runner stops the wait earlier by one unit for every further reply that comes in, so the
        // turn stays inside `D` however many answer. (A reserve for every candidate instead
        // shrank the window by the cost of replies that then never arrived in it.)
        let fuse_ns = u64::try_from(FUSE_RESERVE.as_nanos()).unwrap_or(8_000);
        let unit_ns = self.admit_unit_ns.min(UNIT_CAP_NS);
        let reserve_ns = fuse_ns + unit_ns * 2;
        let plan = {
            let mut s = lock(&sps.sched);
            let plan = s.plan_with(
                &trig,
                TurnTimes {
                    now_us: now / 1000,
                    spent_us: Some(spent_us),
                },
                PlaceMode::Own,
                Some(u32::try_from(reserve_ns / 1000).unwrap_or(u32::MAX)),
            );
            s.end_key(mask);
            plan
        };
        self.turn_seq += 1;
        let mut slots: Vec<Option<Job>> = jobs.into_iter().map(Some).collect();
        let mut planned: Vec<PlannedJob> = Vec::with_capacity(slots.len());
        let kappa = KAPPA_PROVISIONAL;
        let mut order: Vec<(usize, Option<Assignment>)> = plan
            .assignments()
            .iter()
            .map(|a| (a.pid, Some(*a)))
            .collect();
        // Jobs the scheduler does not track (more than 32 plugins) go out as background class.
        for j in slots.iter().flatten() {
            if !order.iter().any(|(p, _)| *p == j.plugin) {
                order.push((j.plugin, None));
            }
        }
        for (pid, a) in order {
            let Some(job) = slots
                .iter_mut()
                .find(|j| j.as_ref().is_some_and(|j| j.plugin == pid))
                .and_then(Option::take)
            else {
                continue;
            };
            let refinement = self
                .plugins
                .get(pid)
                .is_some_and(|p| p.priority == Priority::Refinement);
            let busy = trig.iter().any(|t| t.pid == pid && t.busy);
            let grant_us = a.map_or(5, |a| (a.grant_fuel / kappa).clamp(5, 200));
            let grant = FuelGrant::for_time(Duration::from_micros(grant_us), kappa)
                .with_wall(B2_INTERACTIVE);
            let Some(mut d) = self.new_job(pid, &job, true, grant) else {
                continue;
            };
            if busy || refinement || a.is_none() {
                d.interactive = false;
            }
            let hold = a
                .filter(|a| wait && d.interactive && a.hold_us > 0)
                .map(|a| Duration::from_micros(u64::from(a.hold_us)));
            if let (Some(a), true, Some(info)) = (a, d.interactive, self.jobs.get_mut(&d.job)) {
                info.plan = Some(PlanInfo {
                    a,
                    waited: hold.is_some(),
                    sent_ns: now,
                    t0_ns,
                    clamp_ns: t0_ns + 300_000 - reserve_ns,
                    reserve_us: u32::try_from(reserve_ns / 1000).unwrap_or(u32::MAX),
                    spent_us,
                    turn_seq: self.turn_seq,
                });
            }
            planned.push(PlannedJob { job: d, hold });
        }
        if planned.is_empty() {
            return Handoff::default();
        }
        let sink: Arc<dyn ReplySink> = Arc::clone(&self.sink) as Arc<dyn ReplySink>;
        let turn = Turn::adaptive(t0_ns, wait, fuse_ns, unit_ns);
        let before_send = gorca_core::plugin_port::now_ns();
        let h = self.runner.interactive_planned(&turn, planned, &sink);
        self.last_end_ns = gorca_core::plugin_port::now_ns();
        if tracing::enabled!(tracing::Level::DEBUG) {
            let end = gorca_core::plugin_port::now_ns();
            tracing::debug!(
                snapshot_to_plan_us = spent_us,
                plan_and_build_us = before_send.saturating_sub(now) / 1000,
                send_and_wait_us = end.saturating_sub(before_send) / 1000,
                turn_so_far_us = end.saturating_sub(t0_ns) / 1000,
                asked = h.asked,
                arrived = h.arrived,
                "sps turn"
            );
        }
        Handoff {
            asked: h.asked,
            arrived: h.arrived,
            waited_ns: u64::try_from(h.waited.as_nanos()).unwrap_or(u64::MAX),
        }
    }

    fn masked_now(p: &ScopePlugin) -> bool {
        matches!(
            p.slot.state(),
            SlotState::Backoff { .. } | SlotState::Disabled
        )
    }
}

/// Resolve a message fact from the plugin's catalog in the UI language; `None` when the
/// catalog lacks it.
fn resolve_msg(ns: &str, id: &str, args: &[(String, String)]) -> Option<String> {
    let a: Vec<(&str, gorca_i18n::Arg<'_>)> = args
        .iter()
        .map(|(k, v)| (k.as_str(), gorca_i18n::Arg::Str(v.as_str())))
        .collect();
    gorca_i18n::ext::format(ns, id, gorca_i18n::language(), &a).filter(|t| !t.is_empty())
}

/// A plugin executor that resolves `Value::Msg` facts on the worker thread, right after the
/// call, in the UI language of that moment. An id the catalog lacks becomes `Value::Other`
/// (dropped at admission).
struct ResolvingExec {
    inner: PluginExec,
    ns: String,
}

impl Exec for ResolvingExec {
    fn run(&mut self, job: &DeriveJob) -> PluginReply {
        let mut r = self.inner.run(job);
        if let PluginReply::Done { effects, .. } = &mut r {
            for e in effects.iter_mut() {
                if let Effect::Assert { value, .. } = e
                    && let Value::Msg { id, args } = value
                {
                    *value = match resolve_msg(&self.ns, id, args) {
                        Some(t) => Value::Text(t),
                        None => Value::Other,
                    };
                }
            }
        }
        r
    }
    fn reset(&mut self) {
        self.inner.reset();
    }
    fn warm(&mut self) {
        self.inner.warm();
    }
}

/// What a log line may say about a fact value: the kind and, for text, its length. Never the
/// text (message contents, names and secrets must not reach a log).
fn value_shape(v: &FactValue) -> String {
    match v {
        FactValue::Absent => "absent".to_owned(),
        FactValue::Code(c) => format!("code({c})"),
        FactValue::Bits(b) => format!("bits({b:#x})"),
        FactValue::Number(n) => format!("number({n})"),
        FactValue::Text(t) => format!("text(len={})", t.len()),
        FactValue::Truth(b) => format!("truth({b})"),
    }
}

fn effects_of(effects: Vec<Effect>, ns: &str) -> Vec<FactEffect> {
    let mut out = Vec::new();
    for e in effects {
        match e {
            Effect::Assert {
                target: Target::Node(node),
                group,
                layer,
                value,
            } => {
                let value = match value {
                    Value::Absent => FactValue::Absent,
                    Value::Code(c) => FactValue::Code(c),
                    Value::Bits(b) => FactValue::Bits(b),
                    Value::Number(n) => FactValue::Number(n),
                    Value::Text(t) => FactValue::Text(Arc::from(t)),
                    Value::Truth(b) => FactValue::Truth(b),
                    Value::Msg { id, args } => {
                        // Resolved from the plugin's own catalog in the UI language of this
                        // moment; an id the catalog lacks drops the fact.
                        let Some(t) = resolve_msg(ns, &id, &args) else {
                            tracing::debug!(
                                node,
                                group,
                                id_len = id.len(),
                                "plugin message not in its catalog"
                            );
                            continue;
                        };
                        FactValue::Text(Arc::from(t))
                    }
                    Value::Other => continue,
                };
                tracing::debug!(node, group, patch = layer == 0, value = %value_shape(&value), "plugin fact");
                out.push(FactEffect {
                    node,
                    group,
                    value: Some(value),
                    patch: layer == 0,
                });
            }
            Effect::Retract {
                target: Target::Node(node),
                group,
            } => out.push(FactEffect {
                node,
                group,
                value: None,
                patch: true,
            }),
            // The text of a plugin's log line is plugin-derived and may hold page content: only
            // its length reaches the log.
            Effect::Log(level, msg) => tracing::debug!(level, text_len = msg.len(), "plugin log"),
            // Selectors, hide, categories and the R-lane effects are not applied in W (later
            // slices); they are dropped, never an error.
            Effect::Assert { .. }
            | Effect::Retract { .. }
            | Effect::Hide { .. }
            | Effect::Category { .. }
            | Effect::Say { .. }
            | Effect::Cue(_)
            | Effect::Act(_) => {}
        }
    }
    out
}

impl ScopeRunner for PluginScope {
    fn plugins(&self) -> &[PluginMeta] {
        &self.metas
    }

    fn watches(&self, row: &Row) -> Vec<(usize, u16)> {
        let mut out = Vec::new();
        if row.secret {
            return out;
        }
        for (i, p) in self.plugins.iter().enumerate() {
            if let Some(w) = p.watches.iter().find(|w| {
                w.matches(
                    row.tag.as_deref(),
                    row.class.as_deref(),
                    row.aria_role.as_deref(),
                )
            }) {
                out.push((i, w.id));
            }
        }
        out
    }

    fn interactive(&mut self, t0_ns: u64, wait: bool, jobs: Vec<Job>) -> Handoff {
        let n = jobs.len();
        if n == 0 {
            return Handoff::default();
        }
        if let Some(sps) = self.sps.clone() {
            return self.interactive_sps(&sps, t0_ns, wait, jobs);
        }
        // Progressive filling over what is left of D after the snapshot and the send.
        let spent = Duration::from_nanos(gorca_core::plugin_port::now_ns().saturating_sub(t0_ns));
        let pool = Duration::from_micros(300)
            .saturating_sub(spent)
            .saturating_sub(C_MAX * u32::try_from(n).unwrap_or(1));
        let predicted: Vec<Duration> = jobs
            .iter()
            .map(|j| {
                Duration::from_secs_f64(self.plugins.get(j.plugin).map_or(0.0, |p| p.exec_ewma))
                    .min(B2_INTERACTIVE)
            })
            .collect();
        let grants = progressive_fill(pool, B2_INTERACTIVE, &predicted, self.rotate);
        self.rotate = self.rotate.wrapping_add(1);
        // A plugin still busy with an earlier job cannot answer inside this wait, and a
        // `refinement` plugin never gates a turn: their jobs go to the background slot (they run
        // next, newest wins) and stay out of the countdown, so the turn does not wait the full
        // window for them. Their facts arrive as refinements.
        let busy: Vec<bool> = jobs
            .iter()
            .map(|j| {
                self.plugins
                    .get(j.plugin)
                    .is_some_and(|p| p.priority == Priority::Refinement)
                    || self.jobs.values().any(|i| i.plugin == j.plugin)
            })
            .collect();
        let mut djobs = Vec::with_capacity(n);
        for ((job, g), busy) in jobs.iter().zip(grants).zip(busy) {
            let grant = FuelGrant::for_time(g.max(Duration::from_micros(5)), KAPPA_PROVISIONAL)
                .with_wall(B2_INTERACTIVE);
            if let Some(mut d) = self.new_job(job.plugin, job, true, grant) {
                if busy {
                    d.interactive = false;
                }
                djobs.push(d);
            }
        }
        if djobs.is_empty() {
            return Handoff::default();
        }
        let sink: Arc<dyn ReplySink> = Arc::clone(&self.sink) as Arc<dyn ReplySink>;
        let (inter, bg): (Vec<DeriveJob>, Vec<DeriveJob>) =
            djobs.into_iter().partition(|d| d.interactive);
        if !bg.is_empty() {
            self.runner.background(bg, &sink);
        }
        let turn = Turn::starting_at(t0_ns, wait);
        let h = self.runner.interactive(&turn, inter, &sink);
        Handoff {
            asked: h.asked,
            arrived: h.arrived,
            waited_ns: u64::try_from(h.waited.as_nanos()).unwrap_or(u64::MAX),
        }
    }

    fn background(&mut self, jobs: Vec<Job>) {
        let mut djobs = Vec::with_capacity(jobs.len());
        for job in &jobs {
            let grant = FuelGrant::for_time(B1_BACKGROUND, KAPPA_PROVISIONAL);
            if let Some(d) = self.new_job(job.plugin, job, false, grant) {
                djobs.push(d);
            }
        }
        if !djobs.is_empty() {
            let sink: Arc<dyn ReplySink> = Arc::clone(&self.sink) as Arc<dyn ReplySink>;
            self.runner.background(djobs, &sink);
        }
    }

    fn poll(&mut self, out: &mut Vec<Reply>) {
        self.poll_inner(out, false);
    }

    fn poll_waited(&mut self, out: &mut Vec<Reply>) {
        // Only a scope with a wait set (SPS) tells waited jobs from the rest; the interim policy
        // takes everything that arrived, as before.
        self.poll_inner(out, self.sps.is_some());
    }

    fn turn_done(&mut self, admit_ns: u64, replies: u32) {
        if replies == 0 || self.sps.is_none() {
            return;
        }
        // Rises at once to what the last turn cost, decays by 1/16 per turn: the reserve covers
        // the tail, not the mean.
        let unit = admit_ns / (u64::from(replies) + 1);
        let fall = self.admit_unit_ns - self.admit_unit_ns / 16;
        self.admit_unit_ns = unit.max(fall);
    }

    fn outstanding(&self) -> bool {
        // A plugin in backoff keeps the writer ticking so the expiry is noticed.
        !self.jobs.is_empty()
            || !self.synthetic.is_empty()
            || self
                .plugins
                .iter()
                .any(|p| matches!(p.slot.state(), SlotState::Backoff { .. }))
    }
}

impl PluginScope {
    /// The replies that arrived, as core replies. With `waited_only`, only those of jobs the
    /// last handoff waited for; the others stay in the mailbox and the writer is woken again to
    /// take them in a later turn.
    fn poll_inner(&mut self, out: &mut Vec<Reply>, waited_only: bool) {
        out.append(&mut self.synthetic);
        let now = self.now();
        // A backoff that has run out lifts the mask; the core re-derives everything watched.
        for (i, p) in self.plugins.iter_mut().enumerate() {
            if let SlotState::Backoff { until } = p.slot.state()
                && now >= until
            {
                let _ = p.slot.event(Event::BackoffExpired, now);
                if p.masked {
                    p.masked = false;
                    tracing::info!(plugin = %p.meta.name, "plugin backoff over: unmasked");
                    out.push(Reply {
                        plugin: i,
                        t: 0,
                        targets: Vec::new(),
                        body: ReplyBody::Unmasked,
                    });
                }
            }
        }
        let replies = std::mem::take(&mut *lock(&self.sink.q));
        let mut observed: Vec<(usize, PlanInfo, ReplyTiming, u32, bool)> = Vec::new();
        let mut deferred = Vec::new();
        for (r, timing) in replies {
            let id = match &r {
                PluginReply::Done { job, .. }
                | PluginReply::Fault { job, .. }
                | PluginReply::Cancelled { job, .. } => *job,
            };
            if waited_only
                && !self
                    .jobs
                    .get(&id)
                    .is_some_and(|i| i.plan.is_some_and(|p| p.waited))
            {
                deferred.push((r, timing));
                continue;
            }
            let Some(info) = self.jobs.remove(&id) else {
                continue;
            };
            let Some(p) = self.plugins.get_mut(info.plugin) else {
                continue;
            };
            let was_masked = p.masked;
            let mut stale = false;
            // Out of fuel on an interactive grant below the per-call cap is crowding, not
            // misbehaviour (ADR-0026 section 2.4.2 step 4): no strike; the nodes go to the
            // background slice with its larger grant (the refinement path).
            let crowded = info.interactive
                && info.grant < B2_INTERACTIVE
                && matches!(
                    r,
                    PluginReply::Fault {
                        kind: gorca_plugin_host::slot::FaultKind::OutOfFuel,
                        ..
                    }
                );
            let events = if crowded {
                vec![Event::Cancelled]
            } else {
                r.events()
            };
            for ev in events {
                let before = p.slot.stale_replies();
                let _ = p.slot.on_reply(info.slot_epoch, ev, now);
                stale |= p.slot.stale_replies() != before;
            }
            let body = match r {
                PluginReply::Done {
                    effects,
                    reads,
                    exec,
                    ..
                } if !stale => {
                    if let (Some(plan), Some(t)) = (info.plan, timing) {
                        observed.push((
                            info.plugin,
                            plan,
                            t,
                            u32::try_from(exec.as_micros()).unwrap_or(u32::MAX),
                            !effects.is_empty(),
                        ));
                    }
                    tracing::debug!(
                        plugin = %p.meta.name,
                        exec_us = u64::try_from(exec.as_micros()).unwrap_or(u64::MAX),
                        grant_us = u64::try_from(info.grant.as_micros()).unwrap_or(u64::MAX),
                        "plugin call"
                    );
                    p.exec_ewma = 0.8 * p.exec_ewma + 0.2 * exec.as_secs_f64();
                    ReplyBody::Done {
                        effects: effects_of(effects, &p.meta.name),
                        reads: reads.nodes,
                        overflow: reads.overflow,
                    }
                }
                // A reply of a dead incarnation or a displaced job: its nodes run again.
                PluginReply::Done { .. } | PluginReply::Cancelled { .. } => {
                    ReplyBody::Failed { requeue: true }
                }
                PluginReply::Fault { .. } if crowded => {
                    tracing::debug!(plugin = %p.meta.name, "interactive grant too small: refine in the background");
                    ReplyBody::Failed { requeue: true }
                }
                PluginReply::Fault { kind, .. } => {
                    tracing::warn!(plugin = %p.meta.name, ?kind, "plugin call faulted");
                    ReplyBody::Failed { requeue: false }
                }
            };
            let masked = Self::masked_now(p);
            out.push(Reply {
                plugin: info.plugin,
                t: info.t,
                targets: info.targets,
                body,
            });
            if masked && !was_masked {
                p.masked = true;
                tracing::warn!(plugin = %p.meta.name, state = ?p.slot.state(), "plugin masked");
                out.push(Reply {
                    plugin: info.plugin,
                    t: 0,
                    targets: Vec::new(),
                    body: ReplyBody::Masked,
                });
            } else if !masked && was_masked {
                p.masked = false;
                out.push(Reply {
                    plugin: info.plugin,
                    t: 0,
                    targets: Vec::new(),
                    body: ReplyBody::Unmasked,
                });
            }
        }
        if !deferred.is_empty() {
            lock(&self.sink.q).extend(deferred);
            // They were posted before this drain: nothing else will wake the writer for them.
            (self.sink.wake)();
        }
        // What the replies taught the scheduler: start latency (hot or cold), guest time,
        // response time against the hold, usefulness (ADR-0026 section 2.4.7.6).
        let (turn_now, last_end) = (self.turn_seq, self.last_end_ns);
        if let (Some(sps), false) = (&self.sps, observed.is_empty()) {
            let mut s = lock(&sps.sched);
            for (plugin, plan, t, exec_us, useful) in observed {
                tracing::debug!(
                    plugin,
                    cold = t.cold,
                    plan_to_send_us = t.sent_ns.saturating_sub(plan.sent_ns) / 1000,
                    start_us = u64::try_from(t.start.as_micros()).unwrap_or(u64::MAX),
                    exec_us,
                    pickup_to_reply_us = t.at_ns.saturating_sub(t.picked_ns) / 1000,
                    resp_us = t.at_ns.saturating_sub(plan.sent_ns) / 1000,
                    hold_us = plan.a.hold_us,
                    deadline_us = plan.a.deadline_us,
                    waited = plan.waited,
                    reserve_us = plan.reserve_us,
                    spent_us = plan.spent_us,
                    arrive_rel_t0_us = t.at_ns.saturating_sub(plan.t0_ns) / 1000,
                    clamp_rel_t0_us = plan.clamp_ns.saturating_sub(plan.t0_ns) / 1000,
                    wait_ended_rel_t0_us = if plan.turn_seq == turn_now {
                        last_end.saturating_sub(plan.t0_ns) / 1000
                    } else {
                        0
                    },
                    sent_rel_t0_us = t.sent_ns.saturating_sub(plan.t0_ns) / 1000,
                    "sps reply"
                );
                s.observe(&Observation {
                    pid: plugin,
                    state: if t.cold { Start::Cold } else { Start::Hot },
                    start_us: u32::try_from(t.start.as_micros()).unwrap_or(u32::MAX),
                    exec_us,
                    // From the job's own hand-off to the actor: the holds count from the moment
                    // the waited jobs are out, so this is never smaller than what a hold sees.
                    resp_us: u32::try_from(t.at_ns.saturating_sub(t.sent_ns) / 1000)
                        .unwrap_or(u32::MAX),
                    assignment: &plan.a,
                    waited: plan.waited,
                    useful,
                });
            }
        }
    }
}

impl Drop for PluginScope {
    fn drop(&mut self) {
        self.runner.shutdown();
        // `GORCA_SCHED_LOG=<path>`: the input log of the scheduler (numbers only, ADR-0026
        // SPS-6), appended when the scope closes. A diagnostic: it writes one small file on the
        // closing writer.
        if let (Some(path), Some(sps)) = (&self.sched_log, &self.sps)
            && let Ok(s) = sps.sched.lock()
            && let Some(ring) = s.recorded()
        {
            use std::io::Write as _;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
            {
                let _ = writeln!(
                    f,
                    "# scope plugins={} entries={}",
                    self.plugins.len(),
                    ring.total()
                );
                let _ = f.write_all(ring.to_text().as_bytes());
            }
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::disallowed_methods,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::single_match,
    clippy::needless_pass_by_value,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]
mod tests {
    use super::*;

    /// A tracing writer that keeps everything written to it.
    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Capture {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            lock(&self.0).extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
        type Writer = Capture;
        fn make_writer(&'a self) -> Capture {
            self.clone()
        }
    }

    #[test]
    fn no_plugin_derived_text_reaches_a_log() {
        let cap = Capture::default();
        let sub = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(cap.clone())
            .finish();
        let secret = "hunter2-the-message-text";
        tracing::subscriber::with_default(sub, || {
            let out = effects_of(
                vec![
                    Effect::Assert {
                        target: Target::Node(3),
                        group: 1,
                        layer: 0,
                        value: Value::Text(secret.to_owned()),
                    },
                    Effect::Assert {
                        target: Target::Node(3),
                        group: 0,
                        layer: 0,
                        value: Value::Code(5),
                    },
                    Effect::Log(2, secret.to_owned()),
                ],
                "gorca.test",
            );
            assert_eq!(out.len(), 2);
        });
        let log = String::from_utf8(lock(&cap.0).clone()).unwrap();
        assert!(
            log.contains("plugin fact"),
            "the fact line is logged: {log}"
        );
        assert!(log.contains("text(len=24)"), "{log}");
        assert!(log.contains("code(5)"), "{log}");
        assert!(log.contains("text_len=24"), "{log}");
        assert!(
            !log.contains(secret),
            "plugin text leaked into a log: {log}"
        );
        assert!(!log.contains("hunter2"), "{log}");
    }

    #[test]
    fn pins_parse_and_bad_lines_are_ignored() {
        let h = "ab".repeat(32);
        let pins = parse_pins(&format!("# c\n\ngorca.x {h}\nshort 12\nbad zz\n"));
        assert_eq!(pins.len(), 1);
        assert_eq!(pins[0].id, "gorca.x");
        assert_eq!(pins[0].sha256[0], 0xab);
    }

    #[test]
    fn progressive_filling_serves_cheap_plugins_first_and_is_fair() {
        let us = Duration::from_micros;
        // Pool 200 us, three plugins predicted 10, 10 and 300 (capped to 200).
        let g = progressive_fill(us(200), us(200), &[us(10), us(10), us(200)], 0);
        assert_eq!(g.len(), 3);
        // The sum of what the predictions will use never exceeds the pool.
        assert!(g[0] >= us(10) && g[1] >= us(10));
        // The expensive one gets what the cheap ones leave: more than an equal third.
        assert!(g[2] > us(66));
        assert!(g.iter().all(|x| *x <= us(200)));
        // A single plugin gets min(cap, pool).
        assert_eq!(
            progressive_fill(us(150), us(200), &[us(5)], 0),
            vec![us(150)]
        );
        assert!(progressive_fill(us(0), us(200), &[], 0).is_empty());
    }

    #[test]
    fn equal_predictions_get_equal_grants_whatever_the_rotation() {
        let us = Duration::from_micros;
        let pred = [us(100), us(100), us(100)];
        for rot in 0..6 {
            let g = progressive_fill(us(90), us(200), &pred, rot);
            assert!(g.iter().all(|x| *x == g[0]), "rotation {rot}: {g:?}");
            assert!(g.iter().map(Duration::as_micros).sum::<u128>() <= 90);
        }
    }

    // ---- C-1: a backed-off plugin is served again when the backoff is over -------------------

    /// Faults while `fails` is positive, then answers with a name fact.
    struct Flaky {
        fails: Arc<std::sync::atomic::AtomicU32>,
    }

    impl Exec for Flaky {
        fn run(&mut self, job: &DeriveJob) -> PluginReply {
            use std::sync::atomic::Ordering::Relaxed;
            if self.fails.load(Relaxed) > 0 {
                self.fails.fetch_sub(1, Relaxed);
                return PluginReply::Fault {
                    plugin: job.plugin,
                    job: job.job,
                    slot_epoch: job.slot_epoch,
                    kind: gorca_plugin_host::slot::FaultKind::Trap,
                    exec: Duration::ZERO,
                };
            }
            let nodes: Vec<u32> = job.nodes.iter().map(|d| d.index).collect();
            PluginReply::Done {
                plugin: job.plugin,
                job: job.job,
                slot_epoch: job.slot_epoch,
                effects: nodes
                    .iter()
                    .map(|n| Effect::Assert {
                        target: Target::Node(*n),
                        group: 1,
                        layer: 0,
                        value: Value::Text("after backoff".to_owned()),
                    })
                    .collect(),
                reads: ReadSet {
                    nodes,
                    overflow: false,
                },
                exec: Duration::ZERO,
                queued: Duration::ZERO,
                fuel_used: 0,
                wall_overrun: false,
            }
        }
        fn reset(&mut self) {}
    }

    #[test]
    fn a_plugin_in_backoff_gets_jobs_again_when_the_backoff_is_over() {
        let fails = Arc::new(std::sync::atomic::AtomicU32::new(3));
        let f2 = Arc::clone(&fails);
        let execs: Vec<(u32, Box<dyn Exec>)> = vec![(
            0,
            Box::new(Flaky {
                fails: Arc::clone(&fails),
            }),
        )];
        let maker: Box<dyn Fn(u32) -> Option<Box<dyn Exec>> + Send + Sync> = Box::new(move |_| {
            Some(Box::new(Flaky {
                fails: Arc::clone(&f2),
            }) as Box<dyn Exec>)
        });
        let runner = ActorRunner::new(execs, maker, ActorConfig::default()).unwrap();
        let plugin = ScopePlugin::new(
            PluginMeta {
                name: Arc::from("p0"),
                tier: PluginTier::FirstParty,
            },
            vec![Watch {
                id: 1,
                tag: None,
                class: Some("w".into()),
                aria_role: None,
            }],
        );
        let scope = PluginScope::from_parts(runner, vec![plugin], Arc::new(|| {}));
        let skew = Arc::clone(&scope.skew);
        let mut bridge = PluginBridge::new(
            Box::new(scope),
            Arc::new(Governor::default()),
            ScopeSpec::default(),
        );
        let tree = One;
        let row = |v: usize| Row {
            name: Some(Arc::from(format!("v{v}"))),
            class: Some(Arc::from("w")),
            ..Row::default()
        };
        let run_until_idle = |b: &mut PluginBridge| {
            let mut ops = Vec::new();
            for _ in 0..60 {
                b.background(&tree);
                ops.extend(b.take_ops());
                std::thread::sleep(Duration::from_millis(1));
            }
            ops
        };
        // Three faults in a window: the slot backs off and the plugin is masked.
        let mut ops = Vec::new();
        for turn in 0..3 {
            bridge.begin_turn();
            bridge.upsert(1, row(turn));
            ops.extend(run_until_idle(&mut bridge));
        }
        bridge.begin_turn();
        bridge.upsert(1, row(9));
        ops.extend(run_until_idle(&mut bridge));
        assert!(
            ops.iter().any(|o| matches!(o, FactOp::Mask { .. })),
            "masked after three faults: {ops:?}"
        );
        assert_eq!(bridge.live_len(), 0);
        // The writer keeps ticking while the backoff runs (it is outstanding work).
        assert!(bridge.has_work());
        // Backoff over (the first level is 30 s): the plugin is unmasked and serves the node again.
        skew.store(31_000_000_000, std::sync::atomic::Ordering::Relaxed);
        bridge.begin_turn();
        bridge.upsert(1, row(10));
        let mut seen = Vec::new();
        let end = Instant::now() + Duration::from_secs(5);
        while Instant::now() < end && !seen.iter().any(|o| matches!(o, FactOp::Patch { .. })) {
            bridge.background(&tree);
            seen.extend(bridge.take_ops());
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(
            seen.iter().any(|o| matches!(o, FactOp::Unmask { .. })),
            "unmasked: {seen:?}"
        );
        assert!(
            seen.iter().any(|o| matches!(o, FactOp::Patch { .. })),
            "served again: {seen:?}"
        );
    }

    // ---- starvation (bake-off finding: a still-running slot must not skip a plugin) ----------

    use gorca_core::plugin_port::{FactOp, PluginBridge, Tree};
    use gorca_plugin_host::instance::ReadSet;

    /// Spins for `work`, then asserts a name fact "<id>:<name of the target>".
    struct Spin {
        id: usize,
        work: Duration,
    }

    impl Exec for Spin {
        fn run(&mut self, job: &DeriveJob) -> PluginReply {
            let t = Instant::now();
            while t.elapsed() < self.work {
                std::hint::spin_loop();
            }
            let mut effects = Vec::new();
            let mut nodes = Vec::new();
            for d in &job.nodes {
                nodes.push(d.index);
                let name = job.view.name(d.index).unwrap_or_default();
                effects.push(Effect::Assert {
                    target: Target::Node(d.index),
                    group: 1,
                    layer: 0,
                    value: Value::Text(format!("p{}:{name}", self.id)),
                });
            }
            PluginReply::Done {
                plugin: job.plugin,
                job: job.job,
                slot_epoch: job.slot_epoch,
                effects,
                reads: ReadSet {
                    nodes,
                    overflow: false,
                },
                exec: t.elapsed(),
                queued: job.queued_at.elapsed(),
                fuel_used: 0,
                wall_overrun: false,
            }
        }
        fn reset(&mut self) {}
    }

    struct One;
    impl Tree for One {
        fn parent(&self, _n: u32) -> Option<u32> {
            None
        }
        fn children(&self, _n: u32) -> Vec<u32> {
            Vec::new()
        }
    }

    /// Ten plugins on one watched node, one of them slower. Every turn changes the node (a new
    /// focus content) and asks for an interactive derivation, 2 ms apart (a fast typist).
    /// Facts-at-first-utterance is the share of turns whose fact was admitted by the end of that
    /// turn's bounded wait; whatever missed it must arrive as a refinement within a bound.
    #[test]
    fn ten_plugins_one_slower_none_starves() {
        const N: usize = 10;
        const TURNS: usize = 250;
        let works: Vec<Duration> = (0..N)
            .map(|i| {
                if i == 7 {
                    Duration::from_micros(260)
                } else {
                    Duration::from_micros(20)
                }
            })
            .collect();
        let execs: Vec<(u32, Box<dyn Exec>)> = works
            .iter()
            .enumerate()
            .map(|(i, w)| {
                let e: Box<dyn Exec> = Box::new(Spin { id: i, work: *w });
                (u32::try_from(i).unwrap(), e)
            })
            .collect();
        let works2 = works.clone();
        let maker: Box<dyn Fn(u32) -> Option<Box<dyn Exec>> + Send + Sync> = Box::new(move |i| {
            let e: Box<dyn Exec> = Box::new(Spin {
                id: i as usize,
                work: *works2.get(i as usize)?,
            });
            Some(e)
        });
        let runner = ActorRunner::new(execs, maker, ActorConfig::default()).unwrap();
        let watch = Watch {
            id: 1,
            tag: None,
            class: Some("w".into()),
            aria_role: None,
        };
        let plugins: Vec<ScopePlugin> = (0..N)
            .map(|i| {
                ScopePlugin::new(
                    PluginMeta {
                        name: Arc::from(format!("p{i}")),
                        tier: PluginTier::FirstParty,
                    },
                    vec![watch.clone()],
                )
            })
            .collect();
        let scope = PluginScope::from_parts(runner, plugins, Arc::new(|| {}));
        let mut bridge = PluginBridge::new(
            Box::new(scope),
            Arc::new(Governor::default()),
            ScopeSpec::default(),
        );
        let row = |v: usize| Row {
            name: Some(Arc::from(format!("v{v}"))),
            class: Some(Arc::from("w")),
            ..Row::default()
        };
        // first_ok[i] counts turns whose fact for plugin i was in by the end of the wait; lag
        // collects, for the others, how long the refinement took.
        let mut first_ok = [0usize; N];
        let mut lag_us: Vec<u128> = Vec::new();
        let mut seen = vec![vec![None::<Instant>; TURNS]; N];
        let tree = One;
        let apply = |ops: Vec<FactOp>, at: Instant, seen: &mut Vec<Vec<Option<Instant>>>| {
            for op in ops {
                if let FactOp::Patch {
                    plugin,
                    write: gorca_fuse::Write::Val(gorca_fuse::Val::Name(n)),
                    ..
                } = op
                    && let Some((p, v)) = n.split_once(':')
                    && let (Ok(pi), Some(vi)) = (
                        p.trim_start_matches('p').parse::<usize>(),
                        v.trim_start_matches('v').parse::<usize>().ok(),
                    )
                    && plugin.starts_with('p')
                    && vi < TURNS
                    && seen[pi][vi].is_none()
                {
                    seen[pi][vi] = Some(at);
                }
            }
        };
        let mut starts = Vec::with_capacity(TURNS);
        let t_begin = Instant::now();
        for turn in 0..TURNS {
            let target = t_begin + Duration::from_millis(2) * u32::try_from(turn).unwrap();
            // Drain refinements until the turn starts.
            while Instant::now() < target {
                bridge.background(&tree);
                apply(bridge.take_ops(), Instant::now(), &mut seen);
                std::thread::sleep(Duration::from_micros(100));
            }
            let t0 = Instant::now();
            starts.push(t0);
            bridge.begin_turn();
            bridge.upsert(1, row(turn));
            bridge.interactive(&[1], &tree, gorca_core::plugin_port::now_ns());
            apply(bridge.take_ops(), Instant::now(), &mut seen);
        }
        // Let the stragglers finish.
        let end = Instant::now() + Duration::from_millis(500);
        while Instant::now() < end && bridge.has_work() {
            bridge.background(&tree);
            apply(bridge.take_ops(), Instant::now(), &mut seen);
            std::thread::sleep(Duration::from_micros(200));
        }
        for i in 0..N {
            for (turn, t0) in starts.iter().enumerate() {
                match seen[i][turn] {
                    Some(at) => {
                        let lag = at.saturating_duration_since(*t0);
                        if lag <= Duration::from_micros(600) {
                            first_ok[i] += 1;
                        } else {
                            lag_us.push(lag.as_micros());
                        }
                    }
                    // Superseded by a newer turn before it ran is fine (latest wins), but a
                    // plugin that never delivered anything is starved.
                    None => {}
                }
            }
        }
        let delivered: Vec<usize> = (0..N)
            .map(|i| seen[i].iter().filter(|s| s.is_some()).count())
            .collect();
        eprintln!(
            "first-utterance share per plugin: {:?}",
            first_ok
                .iter()
                .map(|c| *c as f64 / TURNS as f64)
                .collect::<Vec<_>>()
        );
        eprintln!(
            "delivered per plugin: {delivered:?}; late lags (us) max {:?}",
            lag_us.iter().max()
        );
        // The gate needs a quiet host (CLAUDE.md: label the conditions, never delete a failing
        // number). On a loaded one the wake latency of ten parked threads dominates, so only the
        // no-starvation properties below are asserted and the run is labelled.
        let load1: f64 = std::fs::read_to_string("/proc/loadavg")
            .ok()
            .and_then(|s| s.split_whitespace().next().and_then(|v| v.parse().ok()))
            .unwrap_or(0.0);
        let cores = std::thread::available_parallelism().map_or(1, usize::from) as f64;
        // The first-utterance share is a gate of an optimised build on a quiet host.
        let quiet = load1 < cores / 4.0 && !cfg!(debug_assertions);
        eprintln!(
            "host load1 {load1:.1} on {cores} cores: quiet_violated={}",
            !quiet
        );
        for i in (0..N).filter(|i| *i != 7) {
            let share = first_ok[i] as f64 / TURNS as f64;
            if quiet {
                // Every benign plugin: at least 97% of turns with the fact in by the end of the
                // wait (600 us: the 0.3 ms deadline plus the wake slack).
                assert!(share >= 0.97, "plugin {i} first-utterance {share:.3}");
            }
            // A loaded host: the share is printed, never asserted (it measures the scheduler).
        }
        // The slower one is never starved: its newest job always runs next, so it delivers for
        // nearly every turn (a job superseded by a newer one is replaced by that one's fact),
        // and no refinement waits unboundedly.
        assert!(
            delivered[7] as f64 / TURNS as f64 >= if load1 < cores { 0.8 } else { 0.4 },
            "slow plugin delivered {} of {TURNS}",
            delivered[7]
        );
        assert!(
            load1 >= cores || lag_us.iter().all(|l| *l < 50_000),
            "a refinement took {:?} us",
            lag_us.iter().max()
        );
    }

    // ---- SPS (ADR-0026 section 2.4.7) --------------------------------------------------------

    /// Records the content of every job it is given, answers with a name fact after `delay`.
    struct Rec {
        seen: Arc<Mutex<Vec<String>>>,
        delay: Duration,
    }

    impl Exec for Rec {
        fn run(&mut self, job: &DeriveJob) -> PluginReply {
            // Everything the plugin side is told about the job, except its ids and clocks.
            lock(&self.seen).push(format!(
                "p{} inter={} epoch={} nodes={:?} fuel={} time={:?} wall={:?}",
                job.plugin,
                job.interactive,
                job.epoch,
                job.nodes,
                job.grant.units,
                job.grant.time,
                job.grant.wall
            ));
            if !self.delay.is_zero() {
                std::thread::sleep(self.delay);
            }
            let nodes: Vec<u32> = job.nodes.iter().map(|d| d.index).collect();
            PluginReply::Done {
                plugin: job.plugin,
                job: job.job,
                slot_epoch: job.slot_epoch,
                effects: nodes
                    .iter()
                    .map(|n| Effect::Assert {
                        target: Target::Node(*n),
                        group: 1,
                        layer: 0,
                        value: Value::Text("fact".to_owned()),
                    })
                    .collect(),
                reads: ReadSet {
                    nodes,
                    overflow: false,
                },
                exec: self.delay.max(Duration::from_micros(1)),
                queued: Duration::ZERO,
                fuel_used: 0,
                wall_overrun: false,
            }
        }
        fn reset(&mut self) {}
    }

    fn sps_scope(
        recs: Vec<(Duration, Priority, TrustTier)>,
        seen: &Arc<Mutex<Vec<String>>>,
    ) -> (PluginScope, Arc<Sps>) {
        let execs: Vec<(u32, Box<dyn Exec>)> = recs
            .iter()
            .enumerate()
            .map(|(i, (d, _, _))| {
                (
                    i as u32,
                    Box::new(Rec {
                        seen: Arc::clone(seen),
                        delay: *d,
                    }) as Box<dyn Exec>,
                )
            })
            .collect();
        let runner = ActorRunner::new(
            execs,
            Box::new(|_| None),
            ActorConfig {
                lost_after: Duration::from_secs(10),
                ..ActorConfig::default()
            },
        )
        .unwrap();
        let plugins: Vec<ScopePlugin> = recs
            .iter()
            .enumerate()
            .map(|(i, (_, p, t))| {
                ScopePlugin::new(
                    PluginMeta {
                        name: Arc::from(format!("p{i}")),
                        tier: core_tier(*t),
                    },
                    vec![Watch {
                        id: 1,
                        tag: None,
                        class: Some("w".into()),
                        aria_role: None,
                    }],
                )
                .with_priority(*p)
            })
            .collect();
        let labels: Vec<(Label, Tier)> = recs
            .iter()
            .map(|(_, p, t)| (label_of(*p), tier_of(*t)))
            .collect();
        let sps = Arc::new(Sps {
            sched: Mutex::new(Scheduler::new(&labels, SchedParams::default())),
        });
        (
            PluginScope::from_parts_sps(runner, plugins, Arc::new(|| {}), Some(Arc::clone(&sps))),
            sps,
        )
    }

    fn w_row(v: usize) -> Row {
        Row {
            name: Some(Arc::from(format!("v{v}"))),
            class: Some(Arc::from("w")),
            ..Row::default()
        }
    }

    #[test]
    fn key_class_ignores_releases_and_modifiers_and_sorts_the_rest() {
        use gorca_input_api::{KeyEvent, Modifiers};
        let ev = |keysym: u32, unichar: u32, modifiers: Modifiers, pressed: bool| KeyEvent {
            keycode: 0,
            keysym,
            unichar,
            modifiers,
            pressed,
        };
        let none = Modifiers::empty();
        assert_eq!(key_class(&ev(0xff09, 0, none, true)), Some(KeyClass::Tab));
        assert_eq!(key_class(&ev(0xff09, 0, none, false)), None, "release");
        assert_eq!(key_class(&ev(0xffe1, 0, none, true)), None, "Shift_L");
        assert_eq!(key_class(&ev(0xffe5, 0, none, true)), None, "Caps_Lock");
        assert_eq!(key_class(&ev(0xff0d, 0, none, true)), Some(KeyClass::Enter));
        assert_eq!(key_class(&ev(0xff54, 0, none, true)), Some(KeyClass::Nav));
        assert_eq!(key_class(&ev(0x61, 0x61, none, true)), Some(KeyClass::Type));
        assert_eq!(
            key_class(&ev(0x61, 0x61, Modifiers::CONTROL, true)),
            Some(KeyClass::Other),
            "a chord is a command, not text"
        );
        assert_eq!(key_class(&ev(0xff1b, 0, none, true)), Some(KeyClass::Other));
    }

    #[test]
    fn the_message_is_the_same_in_and_out_of_the_wait_set() {
        // SPS-8: a plugin cannot tell from the job whether the writer waits for it. Two scopes
        // with the same plugins; in one the writer may wait (the first of its keypress path), in
        // the other the governor refuses (the path already waited), so nothing is waited for.
        let run = |gov: Arc<Governor>, another_writer_waited_first: bool| {
            let seen = Arc::new(Mutex::new(Vec::new()));
            let (scope, _) = sps_scope(
                vec![
                    (Duration::ZERO, Priority::Interactive, TrustTier::FirstParty),
                    (
                        Duration::ZERO,
                        Priority::Unlabelled,
                        TrustTier::TrustedThirdParty,
                    ),
                ],
                &seen,
            );
            let gov2 = Arc::clone(&gov);
            let mut b = PluginBridge::new(Box::new(scope), gov, ScopeSpec::default());
            b.begin_turn();
            b.upsert(1, w_row(1));
            let t0 = gorca_core::plugin_port::now_ns();
            if another_writer_waited_first {
                // The governor has just granted a wait to another writer of this keypress path
                // (same start instant): this one is refused.
                assert!(gov2.claim(t0));
            }
            b.interactive(&[1], &One, t0);
            let end = Instant::now() + Duration::from_secs(3);
            while Instant::now() < end && lock(&seen).len() < 2 {
                b.background(&One);
                std::thread::sleep(Duration::from_millis(1));
            }
            let mut v = lock(&seen).clone();
            v.sort();
            (v, b.stats())
        };
        let (waited, st_a) = run(Arc::new(Governor::default()), false);
        let (unwaited, st_b) = run(Arc::new(Governor::default()), true);
        assert_eq!(st_a.waited, 1);
        assert_eq!(st_b.no_wait, 1, "the second path did not wait");
        assert_eq!(waited.len(), 2);
        assert_eq!(waited, unwaited, "SPS-8: identical messages");
    }

    #[test]
    fn a_refinement_plugin_is_dispatched_in_the_turn_and_never_waited_for() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (scope, sps) = sps_scope(
            vec![
                (Duration::ZERO, Priority::Interactive, TrustTier::FirstParty),
                (
                    Duration::from_millis(60),
                    Priority::Refinement,
                    TrustTier::FirstParty,
                ),
            ],
            &seen,
        );
        let mut b = PluginBridge::new(
            Box::new(scope),
            Arc::new(Governor::default()),
            ScopeSpec::default(),
        );
        b.begin_turn();
        b.upsert(1, w_row(1));
        let t0 = gorca_core::plugin_port::now_ns();
        b.interactive(&[1], &One, t0);
        let took = Duration::from_nanos(gorca_core::plugin_port::now_ns() - t0);
        assert!(
            took < Duration::from_millis(30),
            "waited {took:?} for a refinement plugin"
        );
        let st = b.stats();
        // The refinement plugin is never in the wait set. (A debug build can spend the whole
        // window before the send, then nobody is waited for.)
        assert!(
            st.handoff_asked <= 1,
            "only the interactive plugin may be in the wait set: {st:?}"
        );
        // Its facts arrive later, as a refinement.
        let mut got = Vec::new();
        let end = Instant::now() + Duration::from_secs(5);
        while Instant::now() < end && b.live_len() < 2 {
            b.background(&One);
            got.extend(b.take_ops());
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(b.live_len(), 2, "both plugins derived the row in the end");
        // It was never given a hot place or counted toward the pre-wake.
        let s = lock(&sps.sched);
        assert_eq!(s.armed_mask(0), 0);
        assert!(s.stats(1).is_some());
    }

    #[test]
    fn replies_teach_the_scheduler_their_start_and_exec_times() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (scope, sps) = sps_scope(
            vec![(
                Duration::from_micros(50),
                Priority::Interactive,
                TrustTier::FirstParty,
            )],
            &seen,
        );
        let mut b = PluginBridge::new(
            Box::new(scope),
            Arc::new(Governor::default()),
            ScopeSpec::default(),
        );
        for i in 0..12 {
            b.begin_turn();
            b.upsert(1, w_row(i));
            b.interactive(&[1], &One, gorca_core::plugin_port::now_ns());
            let end = Instant::now() + Duration::from_secs(2);
            while Instant::now() < end && b.has_work() {
                b.background(&One);
                let _ = b.take_ops();
                std::thread::sleep(Duration::from_millis(1));
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        let s = lock(&sps.sched);
        let sum = s.summary(0).unwrap();
        assert!(sum.replies >= 8, "{sum:?}");
        assert!(sum.exec_p50_us >= 50, "{sum:?}");
        assert_eq!(s.fallback(), None);
    }

    /// Counts the DP-1 hook calls.
    #[derive(Debug, Default)]
    struct CountingHook(std::sync::atomic::AtomicU64);

    impl IdleStateHook for CountingHook {
        fn plugins_active(&self, _now_ns: u64) {
            self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    #[test]
    fn a_key_down_arms_workers_under_sps_and_calls_the_idle_hook_point() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (scope, sps) = sps_scope(
            vec![
                (Duration::ZERO, Priority::Interactive, TrustTier::FirstParty),
                (Duration::ZERO, Priority::Refinement, TrustTier::FirstParty),
            ],
            &seen,
        );
        let pw = Prewake::default();
        // No hook set: the default is DP-1 option (a), no lease, nothing called.
        pw.register(scope.runner.prewake_handle(), Some(Arc::clone(&sps)));
        pw.key_down_class(KeyClass::Nav);
        let hook = Arc::new(CountingHook::default());
        assert!(pw.set_idle_hook(hook.clone()));
        assert!(!pw.set_idle_hook(hook.clone()), "set once");
        std::thread::sleep(Duration::from_millis(1));
        pw.key_down_class(KeyClass::Nav);
        assert_eq!(hook.0.load(std::sync::atomic::Ordering::Relaxed), 1);
        let armed = lock(&sps.sched).armed_mask(gorca_rt::process::monotonic_ns() / 1000);
        assert_eq!(
            armed, 0b01,
            "only the interactive plugin, never the refinement one"
        );
        // A typing key learns to wake nothing: many of them, no more hook calls.
        for _ in 0..80 {
            pw.key_down_class(KeyClass::Type);
        }
        let before = hook.0.load(std::sync::atomic::Ordering::Relaxed);
        for _ in 0..5 {
            pw.key_down_class(KeyClass::Type);
        }
        assert_eq!(hook.0.load(std::sync::atomic::Ordering::Relaxed), before);
        drop(scope);
        pw.key_down_class(KeyClass::Nav);
        assert!(pw.is_empty(), "a closed scope is forgotten");
    }

    #[test]
    fn the_reserve_for_admitting_replies_rises_at_once_and_decays_slowly() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (mut scope, _) = sps_scope(
            vec![(Duration::ZERO, Priority::Interactive, TrustTier::FirstParty)],
            &seen,
        );
        assert_eq!(scope.admit_unit_ns, ADMIT_UNIT_PRIOR_NS);
        // A turn that cost 110 us for ten replies: a unit of 10 us (cost = unit * (n + 1)), at once.
        scope.turn_done(110_000, 10);
        assert_eq!(scope.admit_unit_ns, 10_000);
        // Cheaper turns bring it down by a sixteenth each, not to their own mean.
        scope.turn_done(11_000, 10);
        assert_eq!(scope.admit_unit_ns, 10_000 - 10_000 / 16);
        // One reply that took 12 us is a unit of 6 us: it cannot lower the peak at once.
        scope.turn_done(12_000, 1);
        assert!(scope.admit_unit_ns > 8_000);
        // A turn without replies teaches nothing.
        let before = scope.admit_unit_ns;
        scope.turn_done(500_000, 0);
        assert_eq!(scope.admit_unit_ns, before);
    }

    #[test]
    fn replies_nobody_waited_for_are_admitted_in_a_later_turn() {
        // The refinement plugin answers at once, long before the interactive drain; its reply is
        // still not admitted by the handoff (that would spend the writer's budget on a fact no
        // one waited for), only by the next drain.
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (scope, _) = sps_scope(
            vec![
                (
                    Duration::from_micros(30),
                    Priority::Interactive,
                    TrustTier::FirstParty,
                ),
                (Duration::ZERO, Priority::Refinement, TrustTier::FirstParty),
            ],
            &seen,
        );
        let mut b = PluginBridge::new(
            Box::new(scope),
            Arc::new(Governor::default()),
            ScopeSpec::default(),
        );
        b.begin_turn();
        b.upsert(1, w_row(1));
        b.interactive(&[1], &One, gorca_core::plugin_port::now_ns());
        // The interactive plugin may or may not make the (debug build's) window; the refinement
        // one never is admitted by the handoff, however early its reply is in.
        assert!(
            b.live_len() <= 1,
            "only a waited plugin is admitted by the handoff"
        );
        let end = Instant::now() + Duration::from_secs(3);
        while Instant::now() < end && b.live_len() < 2 {
            b.drain();
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(
            b.live_len(),
            2,
            "the refinement fact follows on the next drain"
        );
    }
}
