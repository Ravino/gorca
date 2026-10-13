//! `RealSut`: `b1-fakeapp` processes on an isolated a11y bus, observed through the presentation
//! log of the daemon under test (PW-10, `--bench-present-log`).
//!
//! The daemon itself is started by the driver binary (`b1-walk`); this type only needs to know
//! where its presentation log is. It implements [`Sut`] so the conformance suite and the walk run
//! unchanged against the real thing.
//!
//! Supported when the driver provides them: the mirror probe (needs the test-only debug
//! accessor of SPEC 5.7) and `where_am_i`, which presses the daemon's real command key
//! (Insert+Tab, ADR-0005) through the emulated Mutter ([`RealSut::with_keyboard`]). Not
//! supported: the mirror node count of a retired app. Cases that need them skip (see
//! `Case::needs`).
//!
//! **Listener registration.** A GTK4 fake app emits no object event until the registry tells it
//! that an AT registered its listeners (P9-1), and the registry only says so to apps that are
//! already running when the AT registers. The daemon registers before the walk starts any app,
//! so without help every state, value and name event of Settings and Files is swallowed: S1
//! never fires and "evented" mutations are silent. [`RealConfig::register_listeners`] tells each
//! app right after it starts, like atk-bridge asking the registry on a real desktop.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::appctl::{AppArgs, B1App, read_truth};
use super::keys::{self, FakeMutter};
use super::proto::{AppStats, Cmd, CmdReply, ModelSpec, TruthLine};
use super::sut::{Feature, Presentation, Probe, Source, Sut, SutError, SutResult};
use super::world::state;

/// The test-only debug accessor of the daemon under test (SPEC 5.7): reads of its mirror,
/// addressed by the unique bus name and object path of the fake application.
pub trait MirrorProbe: Send + Sync + std::fmt::Debug {
    fn probe(&self, bus: &str, path: &str) -> Probe;
    /// Live nodes of the mirror of `bus`; `None` once the application's mirror was released.
    fn nodes(&self, bus: &str) -> Option<usize>;
}

/// Where things are.
#[derive(Clone, Debug)]
pub struct RealConfig {
    pub fakeapp_bin: PathBuf,
    pub models_dir: PathBuf,
    /// Private a11y bus.
    pub a11y_bus: String,
    /// Private session bus (the verified Shell owns `org.gnome.Shell` here).
    pub session_bus: Option<String>,
    /// Scratch directory: truth logs, edited models, peer sockets.
    pub work_dir: PathBuf,
    /// The daemon's presentation log.
    pub present_log: PathBuf,
    /// Scale every started app to this many nodes (the `big` run).
    pub scale_nodes: Option<u32>,
    /// Allow a bus that is not a `/gorca-h.*` harness session (the fake app's own tests).
    pub allow_any_bus: bool,
    /// Tell every app that starts that an AT registered its listeners (lifts the P9-1 gate), as
    /// the registry does on a real desktop. Off for the in-process conformance fixture, which
    /// does it itself and tests the deferred case.
    pub register_listeners: bool,
}

#[derive(Debug)]
struct Instance {
    label: String,
    bus: String,
    app: Option<B1App>,
    truth: PathBuf,
    spec: ModelSpec,
}

#[derive(Debug)]
pub struct RealSut {
    cfg: RealConfig,
    arm: String,
    inst: Vec<Instance>,
    n: u32,
    probe: Option<std::sync::Arc<dyn MirrorProbe>>,
    /// The emulated Mutter whose grabs the daemon registered with: the way into `where_am_i`.
    keyboard: Option<FakeMutter>,
    defer_registration: bool,
}

impl RealSut {
    pub fn new(cfg: RealConfig, arm: &str) -> Self {
        Self {
            cfg,
            arm: arm.to_owned(),
            inst: Vec::new(),
            n: 0,
            probe: None,
            keyboard: None,
            defer_registration: false,
        }
    }

    /// Give the SUT the emulated Mutter the daemon has registered its key grabs with: `where_am_i`
    /// then presses the daemon's own command chord.
    #[must_use]
    pub fn with_keyboard(mut self, mutter: FakeMutter) -> Self {
        self.keyboard = Some(mutter);
        self
    }

    /// Attach the debug accessor of an in-process daemon: cases that read the mirror then run.
    #[must_use]
    pub fn with_probe(mut self, probe: std::sync::Arc<dyn MirrorProbe>) -> Self {
        self.probe = Some(probe);
        self
    }

    fn live(&mut self, label: &str) -> SutResult<&mut Instance> {
        self.inst
            .iter_mut()
            .rev()
            .find(|i| i.label == label && i.app.is_some())
            .ok_or_else(|| SutError::Other(format!("no live app {label}")))
    }

    fn spawn(&mut self, spec: ModelSpec, label: &str) -> SutResult<()> {
        self.n += 1;
        let model_path = self
            .cfg
            .work_dir
            .join(format!("{label}-{}.model.json", self.n));
        std::fs::write(
            &model_path,
            serde_json::to_string(&spec).map_err(|e| SutError::Other(e.to_string()))?,
        )
        .map_err(|e| SutError::Other(e.to_string()))?;
        let mut a = AppArgs::new(
            &model_path.to_string_lossy(),
            &format!("{label}-{}", self.n),
            &self.cfg.a11y_bus,
        );
        a.dir = Some(self.cfg.work_dir.clone());
        a.allow_any_bus = self.cfg.allow_any_bus;
        a.register = !self.cfg.allow_any_bus;
        a.session_bus.clone_from(&self.cfg.session_bus);
        a.own_name.clone_from(&spec.owns_name);
        let mut app = B1App::spawn(&self.cfg.fakeapp_bin, &a, &self.cfg.work_dir)
            .map_err(|e| SutError::Other(e.to_string()))?;
        if self.cfg.register_listeners && !self.defer_registration {
            app.cmd_timeout(&Cmd::RegisterListener, Duration::from_secs(10))
                .map_err(|e| SutError::Other(e.to_string()))?;
        }
        let bus = app.bus_name().to_owned();
        let truth = app.truth_path.clone();
        self.inst.push(Instance {
            label: label.to_owned(),
            bus,
            app: Some(app),
            truth,
            spec,
        });
        Ok(())
    }

    fn load_model(&self, model: &str) -> SutResult<ModelSpec> {
        let p = self.cfg.models_dir.join(format!("{model}.json"));
        let text = std::fs::read_to_string(&p)
            .map_err(|e| SutError::Other(format!("{}: {e}", p.display())))?;
        serde_json::from_str(&text).map_err(|e| SutError::Other(format!("{}: {e}", p.display())))
    }

    /// Instances in start order: (label, bus).
    pub fn instances(&self) -> Vec<(String, String)> {
        self.inst
            .iter()
            .map(|i| (i.label.clone(), i.bus.clone()))
            .collect()
    }
}

fn role_from_name(n: &str) -> u32 {
    use super::world::role as r;
    match n {
        "button" => r::PUSH_BUTTON,
        "check_box" => r::CHECK_BOX,
        "toggle_button" => r::TOGGLE_BUTTON,
        "slider" => r::SLIDER,
        "text_entry" => r::ENTRY,
        "list_item" => r::LIST_ITEM,
        "list" => r::LIST,
        "label" => r::LABEL,
        "panel" => r::PANEL,
        "window" => r::FRAME,
        "combo_box" => r::COMBO_BOX,
        "image" => r::IMAGE,
        "terminal" => r::TERMINAL,
        "application" => r::APPLICATION,
        _ => 0,
    }
}

fn source_from(s: &str) -> Option<Source> {
    Some(match s {
        "hit" => Source::Hit,
        "partial" => Source::Partial,
        "stale" => Source::Stale,
        "miss_reply" => Source::MissReply,
        "refine" => Source::Refine,
        "followup" => Source::Followup,
        "s1" => Source::S1,
        _ => return None,
    })
}

/// Parse a PW-10 presentation log (one JSON object per line); bad lines are skipped.
pub fn parse_present_log(text: &str) -> Vec<Presentation> {
    text.lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter_map(|v| {
            let states = v
                .get("states")
                .and_then(|s| s.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str())
                        .filter_map(state::by_name)
                        .fold(0u64, |m, b| m | (1u64 << b))
                })
                .unwrap_or(0);
            Some(Presentation {
                t_ns: v.get("t_ns")?.as_u64()?,
                stimulus_ns: 0,
                utt: v.get("utt_id")?.as_u64()?,
                app: String::new(),
                app_bus: v.get("app_bus")?.as_str()?.to_owned(),
                path: v.get("path")?.as_str()?.to_owned(),
                source: source_from(v.get("source")?.as_str()?)?,
                role: role_from_name(v.get("role").and_then(|r| r.as_str()).unwrap_or("")),
                states,
                name: v
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_owned(),
                value: v.get("value").and_then(|x| match x {
                    serde_json::Value::Null => None,
                    serde_json::Value::String(s) => Some(s.clone()),
                    other => Some(other.to_string()),
                }),
                may_refine: v
                    .get("may_refine")
                    .and_then(|b| b.as_bool())
                    .unwrap_or(false),
                refines: v.get("refines").and_then(|r| r.as_u64()),
            })
        })
        .collect()
}

impl Sut for RealSut {
    fn arm(&self) -> String {
        self.arm.clone()
    }

    fn supports(&self, f: Feature) -> bool {
        // The daemon is real time, has no latency knobs, no cache-fill signal (R0) and no
        // hostile-ref counters on the wire. The probe and the command key are opt-in.
        match f {
            Feature::Probe => self.probe.is_some(),
            Feature::WhereAmI => self.keyboard.is_some(),
            Feature::LatencyKnobs
            | Feature::VirtualTime
            | Feature::CacheFill
            | Feature::Malformed => false,
        }
    }

    fn start_app(&mut self, model: &str, label: &str) -> SutResult<()> {
        self.start_app_with(model, label, &|_| {})
    }

    fn start_app_with(
        &mut self,
        model: &str,
        label: &str,
        edit: &dyn Fn(&mut ModelSpec),
    ) -> SutResult<()> {
        let mut spec = self.load_model(model)?;
        if let Some(n) = self.cfg.scale_nodes {
            spec.nodes_idle = n;
            spec.nodes_peak = n;
        }
        edit(&mut spec);
        // A new instance under a live label replaces it.
        if let Ok(old) = self.live(label)
            && let Some(mut a) = old.app.take()
        {
            a.kill();
        }
        self.spawn(spec, label)
    }

    fn restart_app(&mut self, label: &str) -> SutResult<()> {
        let spec = {
            let i = self
                .inst
                .iter_mut()
                .rev()
                .find(|i| i.label == label)
                .ok_or_else(|| SutError::Other(format!("never started {label}")))?;
            if let Some(mut a) = i.app.take() {
                a.kill();
            }
            i.spec.clone()
        };
        self.spawn(spec, label)
    }

    fn cmd(&mut self, app: &str, c: Cmd) -> SutResult<CmdReply> {
        let i = self.live(app)?;
        if matches!(c, Cmd::Kill | Cmd::Quit) {
            if let Some(mut a) = i.app.take() {
                a.kill();
            }
            return Ok(CmdReply::ok(serde_json::Value::Null));
        }
        let a = i
            .app
            .as_mut()
            .ok_or_else(|| SutError::Other("app is gone".into()))?;
        a.cmd_timeout(&c, Duration::from_secs(30))
            .map_err(|e| SutError::Other(e.to_string()))
    }

    #[allow(clippy::disallowed_methods)] // real time passes in a real run
    fn advance(&mut self, ms: u64) {
        std::thread::sleep(Duration::from_millis(ms));
    }

    fn now_ns(&self) -> u64 {
        // The same `CLOCK_MONOTONIC` the fake apps and the daemon stamp their logs with.
        gorca_rt::monotonic_ns()
    }

    fn where_am_i(&mut self) -> SutResult<Vec<String>> {
        let Some(m) = self.keyboard.as_mut() else {
            return Err(SutError::Other(
                "where-am-i needs the emulated keyboard manager (RealSut::with_keyboard)".into(),
            ));
        };
        let reply = m
            .chord(keys::INSERT, keys::TAB)
            .map_err(|e| SutError::Other(e.to_string()))?;
        if !reply.consumed {
            return Err(SutError::Other(
                "the where-am-i chord reached the application: the daemon holds no grabs".into(),
            ));
        }
        // The daemon's answer is one spoken sentence, not a node presentation, so there is no
        // per-node stimulus to log (it would count as a lost focus).
        Ok(Vec::new())
    }

    fn defer_listener_registration(&mut self, on: bool) {
        self.defer_registration = on;
    }

    fn register_listener(&mut self, app: &str) -> SutResult<()> {
        let i = self.live(app)?;
        i.app
            .as_mut()
            .ok_or_else(|| SutError::Other("app is gone".into()))?
            .cmd_timeout(&Cmd::RegisterListener, Duration::from_secs(10))
            .map(|_| ())
            .map_err(|e| SutError::Other(e.to_string()))
    }

    fn presentations(&self) -> Vec<Presentation> {
        let text = std::fs::read_to_string(&self.cfg.present_log).unwrap_or_default();
        let mut v = parse_present_log(&text);
        let truth = self.truth();
        let label_of: BTreeMap<&str, &str> = self
            .inst
            .iter()
            .map(|i| (i.bus.as_str(), i.label.as_str()))
            .collect();
        for p in &mut v {
            p.app = label_of
                .get(p.app_bus.as_str())
                .map_or("", |l| *l)
                .to_owned();
            // Stimulus: the latest focus or mutation of that node at or before the utterance.
            p.stimulus_ns = truth
                .iter()
                .filter(|l| l.app == p.app && l.path == p.path && l.t_ns <= p.t_ns)
                .map(|l| l.t_ns)
                .max()
                .unwrap_or(p.t_ns);
        }
        v
    }

    fn truth(&self) -> Vec<TruthLine> {
        let mut all: Vec<TruthLine> = Vec::new();
        for i in &self.inst {
            let mut t = read_truth(&i.truth);
            for l in &mut t {
                // The log carries the process label (`settings-3`); the suite speaks labels.
                l.app.clone_from(&i.label);
            }
            all.extend(t);
        }
        // The daemon never logs the text of a secret field; its tags are compared instead.
        if let Some(key) = std::fs::read_to_string(&self.cfg.present_log)
            .ok()
            .and_then(|t| super::verdict::redaction_key(&t))
        {
            super::verdict::redact_truth(&mut all, &key);
        }
        all.sort_by_key(|l| l.t_ns);
        all
    }

    fn probe(&self, app: &str, path: &str) -> Probe {
        match &self.probe {
            Some(p) => p.probe(&self.bus_name(app), path),
            None => Probe::Miss,
        }
    }

    fn mirror_nodes(&self, bus_name: &str) -> Option<usize> {
        self.probe.as_ref().and_then(|p| p.nodes(bus_name))
    }

    fn bus_name(&self, app: &str) -> String {
        self.inst
            .iter()
            .rev()
            .find(|i| i.label == app && i.app.is_some())
            .map(|i| i.bus.clone())
            .unwrap_or_default()
    }

    fn calls(&mut self, app: &str) -> SutResult<AppStats> {
        let i = self.live(app)?;
        i.app
            .as_mut()
            .ok_or_else(|| SutError::Other("app is gone".into()))?
            .stats()
            .map_err(|e| SutError::Other(e.to_string()))
    }

    fn set_latency(&mut self, _event_us: u64, _reply_us: u64) {}
}

/// Models directory of this repository.
pub fn default_models_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/harness/b1/models")
}

/// RSS of a process in KiB (0 when unreadable).
pub fn proc_rss_kib(pid: u32) -> u64 {
    std::fs::read_to_string(format!("/proc/{pid}/status"))
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("VmRSS:"))
                .and_then(|v| v.split_whitespace().next().and_then(|n| n.parse().ok()))
        })
        .unwrap_or(0)
}

/// CPU time of all threads of a process in ns, from `schedstat`.
pub fn proc_cpu_ns(pid: u32) -> u64 {
    let Ok(dir) = std::fs::read_dir(format!("/proc/{pid}/task")) else {
        return 0;
    };
    dir.flatten()
        .filter_map(|t| std::fs::read_to_string(t.path().join("schedstat")).ok())
        .filter_map(|s| {
            s.split_whitespace()
                .next()
                .and_then(|n| n.parse::<u64>().ok())
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_presentation_log_of_the_contract_parses() {
        let l = r#"{"t_ns":5,"utt_id":2,"epoch":1,"app_bus":":1.9","path":"/p","source":"miss_reply","role":"button","states":["checked","showing"],"name":"n","value":42.0,"may_refine":false,"refines":null}
not json
{"t_ns":6,"utt_id":3,"app_bus":":1.9","path":"/p","source":"refine","role":"button","states":[],"name":"m","value":null,"may_refine":false,"refines":2}"#;
        let v = parse_present_log(l);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].source, Source::MissReply);
        assert_eq!(v[0].states, (1 << state::CHECKED) | (1 << state::SHOWING));
        assert_eq!(v[0].value.as_deref(), Some("42.0"));
        assert_eq!(v[1].refines, Some(2));
    }
}
