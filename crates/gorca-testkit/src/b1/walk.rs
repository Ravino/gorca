//! The scripted walk W-1 (SPEC 8.2): deterministic, seeded, human-paced; and the run-directory
//! writer of the contract in `crates/gorca-bench-b1/src/rundir.rs`.
//!
//! The walk is **data first**: [`plan_loop`] turns a [`WalkSpec`] and a seed into a list of
//! [`Timed`] steps; [`execute`] plays it against any [`Sut`] (the virtual-time reference reader,
//! or the daemon-backed one) and counts what it really did. The scripted counts come from the
//! plan, the actual ones from the execution, so a policy or harness bug that makes steps vanish
//! shows up as `actual < scripted` and the run is invalid.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{Value, json};

use super::proto::{Cmd, HangMode};
use super::service::Rng;
use super::sut::{Presentation, Source, Sut, SutError, SutExt, SutResult};
use super::world::{role, state};

// ---- spec and its TOML subset ---------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct WalkSpec {
    pub step_ms_min: u64,
    pub step_ms_max: u64,
    pub burst_hz: u64,
    pub burst_s: u64,
    pub pause_every: u64,
    pub pause_ms_min: u64,
    pub pause_ms_max: u64,
    pub loops: u32,
    pub storm_loop: u32,
    pub hung_loop: u32,
    pub restart_every: u32,
    pub search_results_replaced: u32,
    pub overview_nodes: u32,
    pub results_nodes: u32,
    pub settings_first_focus_ms: u64,
    pub header_tabs: u32,
    pub initial_tabs: u32,
    pub sidebar_arrows: u32,
    pub panels: u32,
    pub panel_tabs: u32,
    pub switches: u32,
    pub slider_steps: u32,
    pub silent_rename_pct: u64,
    pub where_am_i: u32,
    pub panel_nodes: u32,
    pub list_arrows: u32,
    pub page_down: u32,
    pub list_replace_nodes: u32,
    pub revisit_count: u32,
    pub revisit_mutate_pct: u64,
    pub revisit_evented_pct: u64,
    pub l3_rate: u64,
    pub l3_duration_ms: u64,
    pub hung_delay_ms: u64,
}

impl Default for WalkSpec {
    fn default() -> Self {
        Self {
            step_ms_min: 150,
            step_ms_max: 400,
            burst_hz: 30,
            burst_s: 3,
            pause_every: 20,
            pause_ms_min: 2_000,
            pause_ms_max: 5_000,
            loops: 3,
            storm_loop: 1,
            hung_loop: 2,
            restart_every: 3,
            search_results_replaced: 3,
            overview_nodes: 1_200,
            results_nodes: 30,
            settings_first_focus_ms: 50,
            header_tabs: 3,
            initial_tabs: 30,
            sidebar_arrows: 20,
            panels: 5,
            panel_tabs: 30,
            switches: 3,
            slider_steps: 5,
            silent_rename_pct: 10,
            where_am_i: 5,
            panel_nodes: 700,
            list_arrows: 200,
            page_down: 5,
            list_replace_nodes: 300,
            revisit_count: 30,
            revisit_mutate_pct: 50,
            revisit_evented_pct: 70,
            l3_rate: 50_000,
            l3_duration_ms: 20_000,
            hung_delay_ms: 2_000,
        }
    }
}

/// `[section]` / `key = integer` subset of TOML (comments with `#`).
pub fn parse_toml(text: &str) -> Result<BTreeMap<String, BTreeMap<String, i64>>, String> {
    let mut out: BTreeMap<String, BTreeMap<String, i64>> = BTreeMap::new();
    let mut sec = String::new();
    for (n, raw) in text.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if let Some(s) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            s.trim().clone_into(&mut sec);
            out.entry(sec.clone()).or_default();
            continue;
        }
        let (k, v) = line
            .split_once('=')
            .ok_or_else(|| format!("line {}: expected key = value", n + 1))?;
        let v: i64 = v
            .trim()
            .replace('_', "")
            .parse()
            .map_err(|_| format!("line {}: `{}` is not an integer", n + 1, v.trim()))?;
        out.entry(sec.clone())
            .or_default()
            .insert(k.trim().to_owned(), v);
    }
    Ok(out)
}

impl WalkSpec {
    pub fn from_toml(text: &str) -> Result<Self, String> {
        let t = parse_toml(text)?;
        let mut s = Self::default();
        let get = |sec: &str, key: &str| t.get(sec).and_then(|m| m.get(key)).copied();
        macro_rules! set {
            ($f:ident, $sec:literal, $key:literal) => {
                if let Some(v) = get($sec, $key) {
                    s.$f = v
                        .try_into()
                        .map_err(|_| format!("[{}] {} out of range", $sec, $key))?;
                }
            };
        }
        set!(step_ms_min, "pacing", "step_ms_min");
        set!(step_ms_max, "pacing", "step_ms_max");
        set!(burst_hz, "pacing", "burst_hz");
        set!(burst_s, "pacing", "burst_s");
        set!(pause_every, "pacing", "pause_every");
        set!(pause_ms_min, "pacing", "pause_ms_min");
        set!(pause_ms_max, "pacing", "pause_ms_max");
        set!(loops, "loops", "loops");
        set!(storm_loop, "loops", "storm_loop");
        set!(hung_loop, "loops", "hung_loop");
        set!(restart_every, "loops", "restart_every");
        set!(search_results_replaced, "shell", "search_results_replaced");
        set!(overview_nodes, "shell", "overview_nodes");
        set!(results_nodes, "shell", "results_nodes");
        set!(settings_first_focus_ms, "shell", "settings_first_focus_ms");
        set!(header_tabs, "settings", "header_tabs");
        set!(initial_tabs, "settings", "initial_tabs");
        set!(sidebar_arrows, "settings", "sidebar_arrows");
        set!(panels, "settings", "panels");
        set!(panel_tabs, "settings", "panel_tabs");
        set!(switches, "settings", "switches");
        set!(slider_steps, "settings", "slider_steps");
        set!(silent_rename_pct, "settings", "silent_rename_pct");
        set!(where_am_i, "settings", "where_am_i");
        set!(panel_nodes, "settings", "panel_nodes");
        set!(list_arrows, "files", "list_arrows");
        set!(page_down, "files", "page_down");
        set!(list_replace_nodes, "files", "list_replace_nodes");
        set!(revisit_count, "revisit", "count");
        set!(revisit_mutate_pct, "revisit", "mutate_pct");
        set!(revisit_evented_pct, "revisit", "evented_pct");
        set!(l3_rate, "l3", "rate");
        set!(l3_duration_ms, "l3", "duration_ms");
        set!(hung_delay_ms, "hung", "delay_ms");
        if s.step_ms_min > s.step_ms_max || s.pause_ms_min > s.pause_ms_max {
            return Err("min above max".into());
        }
        Ok(s)
    }

    /// `tests/harness/b1/walks/w1.toml` of this repository.
    pub fn w1() -> Result<Self, String> {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/harness/b1/walks/w1.toml");
        let text = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        Self::from_toml(&text)
    }
}

// ---- the plan -------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    Start {
        app: String,
        model: String,
    },
    /// Focus a node; `first` marks the first focus of a freshly started app.
    Focus {
        app: String,
        node: String,
    },
    World {
        app: String,
        cmd: Cmd,
        tag: Tag,
    },
    WhereAmI,
    Kill {
        app: String,
    },
    Restart {
        app: String,
    },
    /// Revisit `count` visited, still live controls of `app`; mutate `mutate_pct` % of them
    /// beforehand, `evented_pct` % of those with an event.
    Revisit {
        app: String,
        count: u32,
        mutate_pct: u64,
        evented_pct: u64,
    },
    Mark(String),
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Tag {
    None,
    Evented,
    Silent,
    S1,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Timed {
    /// Time to wait before the step, ms.
    pub gap_ms: u64,
    pub step: Step,
}

struct Planner<'a> {
    spec: &'a WalkSpec,
    rng: Rng,
    steps: Vec<Timed>,
    n: u64,
}

impl Planner<'_> {
    fn gap(&mut self) -> u64 {
        self.n += 1;
        if self.spec.pause_every > 0 && self.n.is_multiple_of(self.spec.pause_every) {
            self.rng
                .range(self.spec.pause_ms_min, self.spec.pause_ms_max)
        } else {
            self.rng.range(self.spec.step_ms_min, self.spec.step_ms_max)
        }
    }

    fn push(&mut self, step: Step) {
        let gap_ms = self.gap();
        self.steps.push(Timed { gap_ms, step });
    }

    fn push_gap(&mut self, gap_ms: u64, step: Step) {
        self.steps.push(Timed { gap_ms, step });
    }

    fn focus(&mut self, app: &str, node: String) {
        self.push(Step::Focus {
            app: app.to_owned(),
            node,
        });
    }

    fn world(&mut self, app: &str, cmd: Cmd, tag: Tag) {
        self.push(Step::World {
            app: app.to_owned(),
            cmd,
            tag,
        });
    }
}

/// Plan loop `idx` of W-1. Pure: the same spec, seed and index give the same plan.
#[allow(clippy::too_many_lines)]
pub fn plan_loop(spec: &WalkSpec, seed: u64, idx: u32) -> Vec<Timed> {
    let mut p = Planner {
        spec,
        rng: Rng::new(seed ^ (u64::from(idx) << 32) ^ 0x57A1),
        steps: Vec::new(),
        n: 0,
    };
    let storm = idx == spec.storm_loop;
    let hung = idx == spec.hung_loop;
    let restart = spec.restart_every > 0 && (idx + 1).is_multiple_of(spec.restart_every);
    p.push_gap(0, Step::Mark(format!("loop{idx}")));

    // 1. Shell: Super, type "set", Enter.
    p.push(Step::Start {
        app: "shell".into(),
        model: "shell".into(),
    });
    if storm {
        p.push(Step::Start {
            app: "storm".into(),
            model: "storm".into(),
        });
    }
    if hung {
        p.push(Step::Start {
            app: "hung".into(),
            model: "hung".into(),
        });
        p.focus("hung", "window".into());
    }
    p.world(
        "shell",
        Cmd::ReplaceSubtree {
            root: "grid".into(),
            nodes: spec.overview_nodes,
            remove_events: true,
            add_events: true,
        },
        Tag::None,
    );
    p.focus("shell", "search".into());
    for _ in 0..spec.search_results_replaced {
        p.world(
            "shell",
            Cmd::ReplaceSubtree {
                root: "results_root".into(),
                nodes: spec.results_nodes,
                remove_events: true,
                add_events: true,
            },
            Tag::None,
        );
        p.focus("shell", "results[0]".into());
    }
    p.push(Step::Start {
        app: "settings".into(),
        model: "settings".into(),
    });
    let ff = if idx % 2 == 0 {
        spec.settings_first_focus_ms
    } else {
        0
    };
    p.push_gap(
        ff,
        Step::Focus {
            app: "settings".into(),
            node: "header[0]".into(),
        },
    );

    // 2. Settings.
    if storm {
        p.push(Step::World {
            app: "storm".into(),
            cmd: Cmd::Storm {
                rate_per_s: spec.l3_rate,
                duration_ms: spec.l3_duration_ms,
                distinct_nodes: 0,
                targets: Vec::new(),
            },
            tag: Tag::None,
        });
        p.push_gap(0, Step::Mark("l3-start".into()));
    }
    if hung {
        p.push(Step::World {
            app: "hung".into(),
            cmd: Cmd::Hang {
                mode: HangMode::Delay,
                ms: spec.hung_delay_ms,
            },
            tag: Tag::None,
        });
        p.push_gap(0, Step::Mark("hung-start".into()));
    }
    for i in 1..spec.header_tabs {
        p.focus("settings", format!("header[{i}]"));
    }
    for i in 0..spec.initial_tabs {
        p.focus("settings", format!("controls[{i}]"));
    }
    for i in 0..spec.sidebar_arrows {
        p.focus("settings", format!("sidebar[{i}]"));
    }
    // Held arrow: 30 Hz for 3 s over the sidebar.
    let burst = spec.burst_hz * spec.burst_s;
    let gap = 1000 / spec.burst_hz.max(1);
    for i in 0..burst {
        let row = (u64::from(spec.sidebar_arrows) + i) % u64::from(45u32);
        p.push_gap(
            gap,
            Step::Focus {
                app: "settings".into(),
                node: format!("sidebar[{row}]"),
            },
        );
    }
    for panel in 0..spec.panels {
        p.focus("settings", format!("sidebar[{}]", panel + 1));
        p.world(
            "settings",
            Cmd::ReplaceSubtree {
                root: "panel".into(),
                nodes: spec.panel_nodes + panel * 200,
                remove_events: true,
                add_events: true,
            },
            Tag::None,
        );
        for i in 0..spec.panel_tabs {
            p.focus("settings", format!("controls[{i}]"));
        }
        if panel == 2 {
            for j in 0..spec.switches {
                p.focus("settings", format!("switches[{j}]"));
                p.world(
                    "settings",
                    Cmd::SetState {
                        node: format!("switches[{j}]"),
                        state: "checked".into(),
                        on: true,
                        evented: true,
                    },
                    Tag::S1,
                );
            }
            p.focus("settings", "slider[0]".into());
            for k in 1..=spec.slider_steps {
                p.world(
                    "settings",
                    Cmd::SetValue {
                        node: "slider[0]".into(),
                        value: f64::from(k * 10),
                        evented: true,
                    },
                    Tag::S1,
                );
            }
        }
        // Silent computed-name changes on ~10 % of the controls just visited.
        for i in 0..spec.panel_tabs {
            if p.rng.range(0, 99) < spec.silent_rename_pct {
                p.world(
                    "settings",
                    Cmd::Rename {
                        node: format!("controls[{i}]"),
                        name: format!("Panel {panel} control {i} (computed)"),
                        evented: false,
                    },
                    Tag::Silent,
                );
            }
        }
    }
    for k in 0..spec.where_am_i {
        p.focus("settings", format!("controls[{k}]"));
        p.push(Step::WhereAmI);
    }

    // 3. Alt+Tab: switcher rebuild, Files appears.
    p.world("shell", Cmd::RebuildSwitcher { items: 4 }, Tag::None);
    p.world(
        "shell",
        Cmd::Select {
            node: "switcher[0]".into(),
        },
        Tag::None,
    );
    p.world(
        "shell",
        Cmd::Select {
            node: "switcher[1]".into(),
        },
        Tag::None,
    );
    p.push(Step::Start {
        app: "files".into(),
        model: "files".into(),
    });
    p.push_gap(
        spec.settings_first_focus_ms,
        Step::Focus {
            app: "files".into(),
            node: "pathbar[0]".into(),
        },
    );

    // 4. Files.
    for i in 1..3 {
        p.focus("files", format!("pathbar[{i}]"));
    }
    for i in 0..spec.list_arrows {
        let row = i % 60;
        p.focus("files", format!("rows[{row}]"));
        if row == 59 {
            p.world("files", Cmd::Scroll { steps: 60 }, Tag::None);
        }
    }
    for _ in 0..spec.page_down {
        p.world("files", Cmd::Scroll { steps: 60 }, Tag::None);
        p.focus("files", "rows[0]".into());
    }
    p.world(
        "files",
        Cmd::Rename {
            node: "row_labels[3]".into(),
            name: "Renamed by F2.txt".into(),
            evented: true,
        },
        Tag::Evented,
    );
    p.focus("files", "rows[3]".into());
    for _ in 0..2 {
        // Open a folder, then Back: both replace the list.
        p.world(
            "files",
            Cmd::ReplaceSubtree {
                root: "list".into(),
                nodes: spec.list_replace_nodes,
                remove_events: true,
                add_events: true,
            },
            Tag::None,
        );
        p.focus("files", "rows[0]".into());
    }
    p.focus("files", "pathbar[0]".into()); // Ctrl+L
    p.focus("files", "rows[0]".into()); // Escape

    // 5. Alt+Tab back to Settings.
    if restart {
        p.push(Step::Restart {
            app: "settings".into(),
        });
    }
    p.world("shell", Cmd::RebuildSwitcher { items: 4 }, Tag::None);
    p.world(
        "shell",
        Cmd::Select {
            node: "switcher[0]".into(),
        },
        Tag::None,
    );
    p.push(Step::Revisit {
        app: "settings".into(),
        count: spec.revisit_count,
        mutate_pct: spec.revisit_mutate_pct,
        evented_pct: spec.revisit_evented_pct,
    });
    if storm {
        p.push_gap(0, Step::Mark("l3-end".into()));
    }
    if hung {
        p.push_gap(0, Step::Mark("hung-end".into()));
        p.push(Step::World {
            app: "hung".into(),
            cmd: Cmd::Hang {
                mode: HangMode::Off,
                ms: 0,
            },
            tag: Tag::None,
        });
    }
    for app in ["files", "settings", "shell"] {
        p.push_gap(0, Step::Kill { app: app.into() });
    }
    if storm {
        p.push_gap(
            0,
            Step::Kill {
                app: "storm".into(),
            },
        );
    }
    if hung {
        p.push_gap(0, Step::Kill { app: "hung".into() });
    }
    p.steps
}

/// The whole walk: all loops.
pub fn plan(spec: &WalkSpec, seed: u64) -> Vec<Timed> {
    (0..spec.loops)
        .flat_map(|i| plan_loop(spec, seed, i))
        .collect()
}

// ---- counters -------------------------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub focus: u64,
    pub revisits: u64,
    pub evented: u64,
    pub silent: u64,
    pub s1: u64,
    pub ancestry: u64,
    pub app_starts: u64,
}

impl Counts {
    /// `self` reached every scripted count (mutations at revisit are drawn at run time, so the
    /// actual count may exceed the scripted one).
    pub fn covers(&self, scripted: &Counts) -> bool {
        self.focus >= scripted.focus
            && self.revisits >= scripted.revisits
            && self.evented >= scripted.evented
            && self.silent >= scripted.silent
            && self.s1 >= scripted.s1
            && self.ancestry >= scripted.ancestry
            && self.app_starts >= scripted.app_starts
    }

    pub fn to_json(&self) -> Value {
        json!({
            "focus": self.focus, "revisits": self.revisits, "evented_mutations": self.evented,
            "silent_mutations": self.silent, "s1_stimuli": self.s1,
            "ancestry_demands": self.ancestry, "app_starts": self.app_starts,
        })
    }
}

/// What the plan promises (before running it).
pub fn scripted(plan: &[Timed]) -> Counts {
    let mut c = Counts::default();
    for t in plan {
        match &t.step {
            Step::Focus { .. } => c.focus += 1,
            Step::Start { .. } | Step::Restart { .. } => c.app_starts += 1,
            Step::WhereAmI => c.ancestry += 1,
            Step::World {
                tag: Tag::Evented, ..
            } => c.evented += 1,
            Step::World {
                tag: Tag::Silent, ..
            } => c.silent += 1,
            Step::World { tag: Tag::S1, .. } => {
                c.s1 += 1;
                c.evented += 1;
            }
            Step::Revisit { count, .. } => {
                c.revisits += u64::from(*count);
                c.focus += u64::from(*count);
            }
            _ => {}
        }
    }
    c
}

// ---- execution ------------------------------------------------------------------------------

/// Where an instance of an app started.
#[derive(Clone, Debug)]
pub struct Instance {
    pub label: String,
    pub bus: String,
    pub start_ns: u64,
}

#[derive(Clone, Debug, Default)]
pub struct Marks(pub BTreeMap<String, Vec<u64>>);

/// Everything a run produced besides the SUT's own logs.
#[derive(Clone, Debug, Default)]
pub struct WalkRun {
    pub actual: Counts,
    pub scripted: Counts,
    pub instances: Vec<Instance>,
    /// Fake-app call tables, accumulated over instances: label -> `conn/Interface.Member` -> n.
    pub calls: BTreeMap<String, BTreeMap<String, u64>>,
    /// Focus steps: (t_ns, app label, path).
    pub focus_log: Vec<(u64, String, String)>,
    /// Where-am-I stimuli: (t_ns, app label, path) of every node it speaks.
    pub stimuli: Vec<(u64, String, String)>,
    pub marks: Marks,
    pub start_ns: u64,
    pub end_ns: u64,
    pub steps_failed: Vec<String>,
    /// Where-am-I demands during which the focused app served no call (B1-HIT.ancestry).
    pub ancestry_no_ipc: u64,
}

impl WalkRun {
    pub fn mark(&self, name: &str) -> Option<u64> {
        self.marks.0.get(name).and_then(|v| v.last().copied())
    }
}

fn add_calls(
    acc: &mut BTreeMap<String, BTreeMap<String, u64>>,
    label: &str,
    c: &super::proto::AppStats,
) {
    let e = acc.entry(label.to_owned()).or_default();
    for (k, v) in &c.calls {
        *e.entry(k.clone()).or_insert(0) += v;
    }
}

/// How long the reader gets to answer a where-am-I before the app's call counters are read again.
const WHERE_AM_I_SETTLE_MS: u64 = 80;

struct Exec<'a, S: Sut + ?Sized> {
    sut: &'a mut S,
    run: WalkRun,
    rng: Rng,
    /// (app, path) visited since the app's last start, in order.
    visited: Vec<(String, String)>,
    last_focus_app: String,
}

impl<S: Sut + ?Sized> Exec<'_, S> {
    fn fail(&mut self, what: String) {
        self.run.steps_failed.push(what);
    }

    fn start(&mut self, app: &str, model: &str) {
        if let Err(e) = self.sut.start_app(model, app) {
            self.fail(format!("start {app}: {e}"));
            return;
        }
        self.run.actual.app_starts += 1;
        self.run.instances.push(Instance {
            label: app.to_owned(),
            bus: self.sut.bus_name(app),
            start_ns: self.sut.now_ns(),
        });
        self.visited.retain(|(a, _)| a != app);
    }

    fn retire(&mut self, app: &str) {
        if let Ok(st) = self.sut.calls(app) {
            add_calls(&mut self.run.calls, app, &st);
        }
    }

    fn focus(&mut self, app: &str, node: &str, revisit: bool) {
        let path = match self.sut.path_of(app, node) {
            Ok(p) => p,
            Err(e) => return self.fail(format!("focus {app} {node}: {e}")),
        };
        let t = self.sut.now_ns();
        match self.sut.cmd(app, Cmd::Focus { node: path.clone() }) {
            Ok(r) if r.ok => {
                self.run.actual.focus += 1;
                if revisit {
                    self.run.actual.revisits += 1;
                }
                self.run.focus_log.push((t, app.to_owned(), path.clone()));
                app.clone_into(&mut self.last_focus_app);
                if !self.visited.iter().any(|(a, p)| a == app && *p == path) {
                    self.visited.push((app.to_owned(), path));
                }
            }
            Ok(r) => self.fail(format!(
                "focus {app} {node}: {}",
                r.error.unwrap_or_default()
            )),
            Err(e) => self.fail(format!("focus {app} {node}: {e}")),
        }
    }

    fn world(&mut self, app: &str, cmd: &Cmd, tag: Tag) {
        match self.sut.cmd(app, cmd.clone()) {
            Ok(r) if r.ok => match tag {
                Tag::Evented => self.run.actual.evented += 1,
                Tag::Silent => self.run.actual.silent += 1,
                Tag::S1 => {
                    self.run.actual.s1 += 1;
                    self.run.actual.evented += 1;
                }
                Tag::None => {}
            },
            Ok(r) => self.fail(format!("{app} {cmd:?}: {}", r.error.unwrap_or_default())),
            Err(e) => self.fail(format!("{app} {cmd:?}: {e}")),
        }
    }

    fn revisit(&mut self, app: &str, count: u32, mutate_pct: u64, evented_pct: u64) {
        // Candidates: visited since the last start of the app and still alive.
        let mut alive: Vec<String> = Vec::new();
        let cands: Vec<String> = self
            .visited
            .iter()
            .filter(|(a, _)| a == app)
            .map(|(_, p)| p.clone())
            .collect();
        for p in cands.into_iter().rev() {
            if let Ok(r) = self.sut.cmd(app, Cmd::Get { node: p.clone() })
                && r.ok
                && r.data.get("alive").and_then(Value::as_bool) == Some(true)
                && r.data.get("role").and_then(Value::as_u64) != Some(u64::from(role::FRAME))
            {
                alive.push(p);
            }
            if alive.len() >= count as usize {
                break;
            }
        }
        // Mutate about half of them meanwhile (evented 70 %, silent 30 %).
        for p in &alive {
            if self.rng.range(0, 99) < mutate_pct {
                let evented = self.rng.range(0, 99) < evented_pct;
                let tag = if evented { Tag::Evented } else { Tag::Silent };
                let name = format!("Revisit rename {}", self.rng.next_u64() % 1000);
                self.world(
                    app,
                    &Cmd::Rename {
                        node: p.clone(),
                        name,
                        evented,
                    },
                    tag,
                );
            }
        }
        let gap = self.rng.range(150, 400);
        self.sut.advance(gap);
        for p in alive {
            self.focus(app, &p, true);
            let g = self.rng.range(150, 400);
            self.sut.advance(g);
        }
    }

    fn step(&mut self, s: &Step) {
        match s {
            Step::Start { app, model } => self.start(app, model),
            Step::Focus { app, node } => self.focus(app, node, false),
            Step::World { app, cmd, tag } => self.world(app, cmd, *tag),
            Step::WhereAmI => {
                let app = self.last_focus_app.clone();
                let calls_before = self.sut.calls(&app).ok().map(|c| c.total_calls());
                let t = self.sut.now_ns();
                match self.sut.where_am_i() {
                    Ok(paths) => {
                        self.run.actual.ancestry += 1;
                        for p in paths {
                            self.run.stimuli.push((t, app.clone(), p));
                        }
                        // Answered from memory when the app served nothing while the reader
                        // produced the answer.
                        self.sut.advance(WHERE_AM_I_SETTLE_MS);
                        if let (Some(before), Ok(after)) = (calls_before, self.sut.calls(&app))
                            && after.total_calls() <= before
                        {
                            self.run.ancestry_no_ipc += 1;
                        }
                    }
                    Err(e) => self.fail(format!("where-am-i: {e}")),
                }
            }
            Step::Kill { app } => {
                self.retire(app);
                if let Err(e) = self.sut.cmd(app, Cmd::Kill) {
                    self.fail(format!("kill {app}: {e}"));
                }
            }
            Step::Restart { app } => {
                self.retire(app);
                match self.sut.restart_app(app) {
                    Ok(()) => {
                        self.run.actual.app_starts += 1;
                        self.run.instances.push(Instance {
                            label: app.clone(),
                            bus: self.sut.bus_name(app),
                            start_ns: self.sut.now_ns(),
                        });
                        // Only what was visited before the first panel switch has the same path.
                    }
                    Err(e) => self.fail(format!("restart {app}: {e}")),
                }
            }
            Step::Revisit {
                app,
                count,
                mutate_pct,
                evented_pct,
            } => {
                self.revisit(app, *count, *mutate_pct, *evented_pct);
            }
            Step::Mark(m) => {
                let t = self.sut.now_ns();
                self.run.marks.0.entry(m.clone()).or_default().push(t);
            }
        }
    }
}

/// Play `plan` against `sut`. `settle_ms` of quiet time follows so the last replies land.
pub fn execute<S: Sut + ?Sized>(sut: &mut S, spec: &WalkSpec, seed: u64) -> WalkRun {
    execute_for(sut, spec, seed, None)
}

/// [`execute`] that also stops, at the end of a loop, once `max_ns` of the `sut` clock have
/// passed since the start (a soak of fixed duration: the loop count of the spec is the upper
/// bound, not the length). The scripted counts are those of the loops actually played.
pub fn execute_for<S: Sut + ?Sized>(
    sut: &mut S,
    spec: &WalkSpec,
    seed: u64,
    max_ns: Option<u64>,
) -> WalkRun {
    let start_ns = sut.now_ns();
    let mut ex = Exec {
        run: WalkRun {
            start_ns,
            ..WalkRun::default()
        },
        sut,
        rng: Rng::new(seed ^ 0xE8EC),
        visited: Vec::new(),
        last_focus_app: String::new(),
    };
    let mut played: Vec<Timed> = Vec::new();
    for i in 0..spec.loops {
        if max_ns.is_some_and(|m| ex.sut.now_ns().saturating_sub(start_ns) >= m) {
            break;
        }
        for t in plan_loop(spec, seed, i) {
            if t.gap_ms > 0 {
                ex.sut.advance(t.gap_ms);
            }
            ex.step(&t.step);
            // The first focus of a loop's fresh Settings after a restart replays the same seed,
            // so the visited list of the previous instance is kept: dead paths are filtered at
            // revisit.
            played.push(t);
        }
    }
    ex.run.scripted = scripted(&played);
    ex.sut.advance(1_000);
    ex.run.end_ns = ex.sut.now_ns();
    ex.run
}

// ---- run directory --------------------------------------------------------------------------

fn role_name(r: u32) -> &'static str {
    match r {
        role::PUSH_BUTTON => "button",
        role::CHECK_BOX => "check_box",
        role::TOGGLE_BUTTON => "toggle_button",
        role::SLIDER => "slider",
        role::ENTRY => "text_entry",
        role::LIST_ITEM => "list_item",
        role::LIST => "list",
        role::LABEL => "label",
        role::PANEL => "panel",
        role::FRAME => "window",
        role::COMBO_BOX => "combo_box",
        role::IMAGE => "image",
        role::TERMINAL => "terminal",
        role::APPLICATION => "application",
        _ => "unknown",
    }
}

fn source_name(s: Source) -> &'static str {
    match s {
        Source::Hit => "hit",
        Source::Partial => "partial",
        Source::Stale => "stale",
        // S1 is a cold read of the changed node: the contract has no separate source for it.
        Source::MissReply | Source::S1 => "miss_reply",
        Source::Refine => "refine",
        Source::Followup => "followup",
    }
}

fn pres_line(p: &Presentation) -> Value {
    let value = p.value.as_deref().map_or(Value::Null, |v| {
        v.parse::<f64>().map_or_else(|_| json!(v), |n| json!(n))
    });
    json!({
        "t_ns": p.t_ns, "utt_id": p.utt, "epoch": 1, "app_bus": p.app_bus, "path": p.path,
        "source": source_name(p.source), "role": role_name(p.role),
        "states": state::names_of(p.states), "name": p.name, "value": value,
        "may_refine": p.may_refine, "refines": p.refines,
    })
}

/// Write `truth.jsonl`, `present.jsonl`, `counters.json` (and `samples.json`) for a walk run.
///
/// The truth log's `app` is rewritten to the unique bus name of the instance that made the
/// mutation, so the join with the presentation log is by `(app_bus, path)` (`KeyMode::AppAndPath`).
pub fn write_run_dir<S: Sut + ?Sized>(
    dir: &Path,
    sut: &mut S,
    spec: &WalkSpec,
    run: &WalkRun,
) -> std::io::Result<()> {
    write_run_dir_opts(dir, sut, spec, run, true)
}

/// Like [`write_run_dir`]; with `rewrite_present` false `present.jsonl` is left alone (the
/// daemon under test writes it itself, PW-10).
pub fn write_run_dir_opts<S: Sut + ?Sized>(
    dir: &Path,
    sut: &mut S,
    spec: &WalkSpec,
    run: &WalkRun,
    rewrite_present: bool,
) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let mut calls = run.calls.clone();
    // Instances still running.
    let labels: Vec<String> = run.instances.iter().map(|i| i.label.clone()).collect();
    for l in labels.iter().collect::<std::collections::BTreeSet<_>>() {
        if let Ok(st) = sut.calls(l) {
            add_calls(&mut calls, l, &st);
        }
    }
    let bus_at = |label: &str, t: u64| -> String {
        run.instances
            .iter()
            .rfind(|i| i.label == label && i.start_ns <= t)
            .map(|i| i.bus.clone())
            .unwrap_or_else(|| label.to_owned())
    };
    let mut truth = String::new();
    for l in sut.truth() {
        let mut v = serde_json::to_value(&l).unwrap_or_default();
        if let Some(o) = v.as_object_mut() {
            o.insert("app".into(), json!(bus_at(&l.app, l.t_ns)));
        }
        truth.push_str(&v.to_string());
        truth.push('\n');
    }
    for (t, app, path) in &run.stimuli {
        let line = json!({"t_ns": t, "app": bus_at(app, *t), "path": path, "focus": true});
        truth.push_str(&line.to_string());
        truth.push('\n');
    }
    std::fs::write(dir.join("truth.jsonl"), truth)?;
    let pres = sut.presentations();
    let mut text = String::new();
    for p in &pres {
        text.push_str(&pres_line(p).to_string());
        text.push('\n');
    }
    if rewrite_present {
        std::fs::write(dir.join("present.jsonl"), text)?;
    }

    let by_app: BTreeMap<&String, Value> = calls.iter().map(|(a, m)| (a, json!(m))).collect();
    let mut c = serde_json::Map::new();
    let secs = |a: u64, b: u64| b.saturating_sub(a) as f64 / 1e9;
    c.insert("duration_s".into(), json!(secs(run.start_ns, run.end_ns)));
    c.insert("calls".into(), json!(by_app));
    c.insert("scripted".into(), run.scripted.to_json());
    c.insert("actual".into(), run.actual.to_json());
    // B1-HIT.ancestry reads these two at the top level.
    c.insert("ancestry_demands".into(), json!(run.actual.ancestry));
    c.insert("ancestry_no_ipc".into(), json!(run.ancestry_no_ipc));
    if let (Some(a), Some(b)) = (run.mark("l3-start"), run.mark("l3-end")) {
        let storm_calls: u64 = calls
            .get("storm")
            .map(|m| m.values().sum::<u64>())
            .unwrap_or_default();
        // Lost focus stimuli inside the L3 window: no utterance for the node at or after it.
        let lost = run
            .focus_log
            .iter()
            .filter(|(t, ..)| *t >= a && *t <= b)
            .filter(|(t, app, path)| {
                let bus = bus_at(app, *t);
                !pres
                    .iter()
                    .any(|p| p.app_bus == bus && &p.path == path && p.t_ns >= *t)
            })
            .count();
        c.insert("storm_app".into(), json!("storm"));
        c.insert("l3_window_s".into(), json!(secs(a, b)));
        c.insert("l3_calls_to_storm".into(), json!(storm_calls));
        c.insert("l3_lost".into(), json!(lost));
    }
    c.insert("spec_loops".into(), json!(spec.loops));
    std::fs::write(dir.join("counters.json"), Value::Object(c).to_string())?;
    // In real mode the daemon's log is still being written; `b1-walk` uniquifies it at the end.
    uniquify_paths(dir, true, rewrite_present)?;

    // L4: latency of the other apps' focus presentations while `hung` stalls (ms).
    if let (Some(a), Some(b)) = (run.mark("hung-start"), run.mark("hung-end")) {
        let l4: Vec<f64> = pres
            .iter()
            .filter(|p| p.t_ns >= a && p.t_ns <= b && p.app != "hung")
            .filter(|p| matches!(p.source, Source::Hit | Source::MissReply))
            .map(|p| p.t_ns.saturating_sub(p.stimulus_ns) as f64 / 1e6)
            .collect();
        if !l4.is_empty() {
            std::fs::write(dir.join("samples.json"), json!({"L4_ms": l4}).to_string())?;
        }
    }
    Ok(())
}

/// Make node paths unique across apps and app instances by prefixing them with the unique bus
/// name: `<bus><path>`. Restarted apps reuse their paths on purpose (IC-6), and `b1 run` joins
/// the two logs by path alone (`KeyMode::PathOnly`); with this, both join modes agree.
/// Idempotent only on first application: call it once, after the daemon has stopped writing.
pub fn uniquify_paths(dir: &Path, truth: bool, present: bool) -> std::io::Result<()> {
    for (file, key, on) in [
        ("truth.jsonl", "app", truth),
        ("present.jsonl", "app_bus", present),
    ] {
        if !on {
            continue;
        }
        let p = dir.join(file);
        let Ok(text) = std::fs::read_to_string(&p) else {
            continue;
        };
        let mut out = String::with_capacity(text.len() + text.len() / 4);
        for line in text.lines() {
            match serde_json::from_str::<Value>(line) {
                Ok(mut v) => {
                    let prefix = v.get(key).and_then(Value::as_str).unwrap_or("").to_owned();
                    if let Some(path) = v.get("path").and_then(Value::as_str) {
                        let np = format!("{prefix}{path}");
                        v["path"] = json!(np);
                    }
                    out.push_str(&v.to_string());
                }
                Err(_) => out.push_str(line),
            }
            out.push('\n');
        }
        std::fs::write(&p, out)?;
    }
    Ok(())
}

// ---- other run kinds ------------------------------------------------------------------------

/// `ff` run (SPEC 8.3): each model appears `reps` times with first-focus delays {0, 50, 300} ms.
/// Returns first-focus latencies in ms per model.
pub fn run_ff<S: Sut + ?Sized>(sut: &mut S, reps: u32) -> SutResult<BTreeMap<String, Vec<f64>>> {
    let mut out: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    for model in ["shell", "settings", "files", "files-big", "gtk3"] {
        let first = match model {
            "shell" => "clock",
            "gtk3" => "editor",
            _ => "controls[0]",
        };
        let first = if matches!(model, "files" | "files-big") {
            "rows[0]"
        } else {
            first
        };
        for i in 0..reps {
            let delay = [0u64, 50, 300][(i % 3) as usize];
            sut.start_app(model, "ff-app")?;
            sut.advance(delay);
            let path = sut.path_of("ff-app", first)?;
            let before = sut.presentations().len();
            sut.must("ff-app", Cmd::Focus { node: path.clone() })?;
            sut.advance(300);
            let new = sut.presentations().split_off(before);
            if let Some(p) = new.iter().find(|p| p.path == path) {
                out.entry(model.to_owned())
                    .or_default()
                    .push(p.t_ns.saturating_sub(p.stimulus_ns) as f64 / 1e6);
            }
            let _ = sut.cmd("ff-app", Cmd::Kill)?;
            sut.advance(10);
        }
    }
    Ok(out)
}

/// `idle` run: one settled desktop, then `window_s` seconds with nothing but the clock.
/// Returns (window seconds, per-app call delta).
pub fn run_idle<S: Sut + ?Sized>(
    sut: &mut S,
    window_s: u64,
) -> SutResult<(u64, BTreeMap<String, BTreeMap<String, u64>>)> {
    for (m, a) in [("shell", "shell"), ("settings", "settings")] {
        sut.start_app(m, a)?;
    }
    for i in 0..10 {
        let n = format!("controls[{i}]");
        sut.focus("settings", &n)?;
    }
    sut.advance(60_000); // settle: 60 s after the last action
    let before: BTreeMap<String, super::proto::AppStats> = ["shell", "settings"]
        .iter()
        .map(|a| Ok(((*a).to_owned(), sut.calls(a)?)))
        .collect::<SutResult<_>>()?;
    for s in 0..window_s {
        if s == window_s / 2 {
            sut.must(
                "shell",
                Cmd::Rename {
                    node: "clock".into(),
                    name: "Tue 13 Oct 10:42".into(),
                    evented: true,
                },
            )?;
        }
        sut.advance(1_000);
    }
    let mut delta = BTreeMap::new();
    for a in ["shell", "settings"] {
        let now = sut.calls(a)?;
        let b = &before[a];
        let m: BTreeMap<String, u64> = now
            .calls
            .iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    v.saturating_sub(b.calls.get(k).copied().unwrap_or(0)),
                )
            })
            .filter(|(_, v)| *v > 0)
            .collect();
        delta.insert(a.to_owned(), m);
    }
    Ok((window_s, delta))
}

/// Run directory of an `idle` run (SPEC 8.3): logs plus the call deltas of the quiet window.
pub fn write_idle_dir<S: Sut + ?Sized>(
    dir: &Path,
    sut: &mut S,
    window_s: u64,
    delta: &BTreeMap<String, BTreeMap<String, u64>>,
) -> std::io::Result<()> {
    write_run_dir(dir, sut, &WalkSpec::default(), &WalkRun::default())?;
    let apps: Vec<&String> = delta.keys().collect();
    let c = json!({"idle_window_s": window_s, "idle_apps": apps, "calls": delta});
    std::fs::write(dir.join("counters.json"), c.to_string())
}

/// Run directory of an `ff` run: logs plus `samples.json` with the first-focus latencies.
pub fn write_ff_dir<S: Sut + ?Sized>(
    dir: &Path,
    sut: &mut S,
    ff: &BTreeMap<String, Vec<f64>>,
    startup_calls: &BTreeMap<String, u64>,
) -> std::io::Result<()> {
    write_run_dir(dir, sut, &WalkSpec::default(), &WalkRun::default())?;
    std::fs::write(dir.join("samples.json"), json!({"FF_ms": ff}).to_string())?;
    let startup: BTreeMap<&String, Value> = startup_calls
        .iter()
        .map(|(m, n)| (m, json!({"calls": n})))
        .collect();
    let c = json!({"startup": startup});
    std::fs::write(dir.join("counters.json"), c.to_string())
}

/// Errors from the driver binary's own plumbing.
pub fn err(s: impl Into<String>) -> SutError {
    SutError::Other(s.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::b1::sim::{Fault, SimSut};

    fn spec() -> WalkSpec {
        WalkSpec::w1().unwrap()
    }

    #[test]
    fn w1_toml_matches_the_defaults_and_the_spec_counts() {
        assert_eq!(spec(), WalkSpec::default());
        assert!(WalkSpec::from_toml("[pacing]\nstep_ms_min = 500\nstep_ms_max = 100").is_err());
        assert_eq!(parse_toml("[a]\nx = 1_000 # c\n").unwrap()["a"]["x"], 1000);
        assert!(parse_toml("junk").is_err());
    }

    #[test]
    fn the_plan_is_deterministic_and_seed_dependent() {
        let a = plan(&spec(), 7);
        let b = plan(&spec(), 7);
        assert_eq!(a, b);
        assert_ne!(a, plan(&spec(), 8));
        assert!(!a.is_empty());
    }

    #[test]
    fn pacing_is_human() {
        let p = plan_loop(&spec(), 1, 0);
        let gaps: Vec<u64> = p.iter().map(|t| t.gap_ms).filter(|g| *g > 0).collect();
        assert!(
            gaps.iter().all(|g| (33..=5_000).contains(g)),
            "{:?}",
            gaps.iter().max()
        );
        let pauses = gaps.iter().filter(|g| **g >= 2_000).count();
        assert!((10..=60).contains(&pauses), "{pauses} pauses");
        // The held arrow is a 30 Hz burst of 3 s.
        assert!(p.iter().filter(|t| t.gap_ms == 33).count() >= 90);
    }

    #[test]
    fn scripted_counts_follow_the_spec() {
        let s = spec();
        let c = scripted(&plan_loop(&s, 1, 0));
        // 5 panels x 30 tabs + 30 initial + header + 20 sidebar + 90 burst + switches + slider...
        assert!(c.focus > 450, "{c:?}");
        assert_eq!(c.revisits, 30);
        assert_eq!(c.s1, u64::from(s.switches + s.slider_steps));
        assert_eq!(c.ancestry, 5);
        assert!(c.app_starts >= 3);
        assert_eq!(scripted(&plan(&s, 1)).revisits, 90);
    }

    fn short() -> WalkSpec {
        // Same shape, shorter, so the unit tests stay quick; the full walk runs once below.
        WalkSpec {
            loops: 3,
            initial_tabs: 32,
            list_arrows: 70,
            panels: 3,
            l3_duration_ms: 3_000,
            ..WalkSpec::default()
        }
    }

    #[test]
    fn a_correct_reader_completes_every_scripted_step() {
        let mut sut = SimSut::with_fault(Fault::None);
        let run = execute(&mut sut, &short(), 11);
        assert!(run.steps_failed.is_empty(), "{:#?}", run.steps_failed);
        assert!(
            run.actual.covers(&run.scripted),
            "{:?} vs {:?}",
            run.actual,
            run.scripted
        );
        assert_eq!(run.actual.focus, run.scripted.focus);
        assert!(run.mark("l3-start").is_some() && run.mark("hung-start").is_some());
        assert!(
            run.instances
                .iter()
                .filter(|i| i.label == "settings")
                .count()
                >= 4,
            "restarts"
        );
    }

    #[test]
    fn a_walk_with_a_time_bound_stops_at_a_loop_end_and_scripts_only_what_it_played() {
        let mut spec = short();
        spec.loops = 50;
        // One loop first, to know its length on the (virtual) clock.
        let mut one = SimSut::with_fault(Fault::None);
        let mut s1 = short();
        s1.loops = 1;
        let r1 = execute(&mut one, &s1, 5);
        let loop_ns = r1.end_ns - r1.start_ns;
        let mut sut = SimSut::with_fault(Fault::None);
        let run = execute_for(&mut sut, &spec, 5, Some(loop_ns * 5 / 2));
        let played = scripted(&plan_loop(&spec, 5, 0)).focus;
        assert!(
            run.scripted.focus >= 2 * played && run.scripted.focus <= 4 * played,
            "a few loops, not 50: {} per loop, {} played",
            played,
            run.scripted.focus
        );
        assert!(run.actual.covers(&run.scripted), "{:?}", run.actual);
        // Without a bound all loops are played.
        let mut all = SimSut::with_fault(Fault::None);
        spec.loops = 2;
        let run = execute_for(&mut all, &spec, 5, None);
        assert_eq!(run.scripted, scripted(&plan(&spec, 5)));
    }

    #[test]
    fn the_same_seed_gives_the_same_presentations() {
        let a = {
            let mut s = SimSut::with_fault(Fault::None);
            execute(&mut s, &short(), 3);
            s.presentations()
                .iter()
                .map(|p| (p.path.clone(), p.name.clone(), p.source))
                .collect::<Vec<_>>()
        };
        let b = {
            let mut s = SimSut::with_fault(Fault::None);
            execute(&mut s, &short(), 3);
            s.presentations()
                .iter()
                .map(|p| (p.path.clone(), p.name.clone(), p.source))
                .collect::<Vec<_>>()
        };
        assert_eq!(a, b);
    }
}
