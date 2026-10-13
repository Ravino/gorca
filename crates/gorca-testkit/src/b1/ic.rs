//! Invalidation conformance suite IC-1..IC-23 (SPEC 5.7).
//!
//! Each case drives fake apps through [`Cmd`]s and judges what the reader under test presented
//! ([`Sut`]). Rules every case follows:
//!
//! * **No `STALE` anywhere** (verdict 6.2) and, for evented mutations settled >= 50 ms before the
//!   focus, `CORRECT` or `REFINED`; for silent ones any non-`STALE` verdict;
//! * **no vacuous pass**: a case first proves it exercised what it claims, by reading the stale
//!   value out of the reader's mirror ([`Sut::probe`]) before it mutates.
//!
//! A case whose precondition the SUT cannot offer (no probe, no cache fill, no malformed-ref
//! mode) is [`Outcome::Skipped`], never `Pass`.

use super::proto::{Cmd, DestroyEvents, HangMode, ModelSpec, P2pMode, ReorderMode};
use super::sut::{Feature, Presentation, Probe, Source, Sut, SutError, SutExt};
use super::verdict::{self, TruthDb, Verdict};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Outcome {
    Pass,
    Skipped(String),
}

/// A failed case.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Fail(pub String);

impl From<SutError> for Fail {
    fn from(e: SutError) -> Self {
        Fail(e.to_string())
    }
}

pub type CaseResult = Result<Outcome, Fail>;

pub struct Case {
    pub id: &'static str,
    pub title: &'static str,
    /// The case asserts on the reader's mirror through [`Sut::probe`]; a SUT without a probe
    /// skips it (the vacuity guard cannot be evaluated).
    pub needs_probe: bool,
    pub run: fn(&mut dyn Sut) -> CaseResult,
}

impl std::fmt::Debug for Case {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", self.id, self.title)
    }
}

fn need(cond: bool, msg: impl FnOnce() -> String) -> Result<(), Fail> {
    if cond { Ok(()) } else { Err(Fail(msg())) }
}

fn skip(why: &str) -> CaseResult {
    Ok(Outcome::Skipped(why.to_owned()))
}

fn name_of(s: &mut dyn Sut, app: &str, node: &str) -> Result<String, Fail> {
    let r = s.must(
        app,
        Cmd::Get {
            node: node.to_owned(),
        },
    )?;
    Ok(r.str("name").unwrap_or_default().to_owned())
}

fn parent_of(s: &mut dyn Sut, app: &str, node: &str) -> Result<String, Fail> {
    let r = s.must(
        app,
        Cmd::Get {
            node: node.to_owned(),
        },
    )?;
    r.str("parent")
        .map(str::to_owned)
        .ok_or_else(|| Fail(format!("{node} has no parent")))
}

/// Presentations of first utterances with verdicts.
fn verdicts(s: &dyn Sut) -> Vec<(Presentation, Verdict)> {
    verdict::classify_all(&s.presentations(), &s.truth())
}

fn no_stale(s: &dyn Sut) -> Result<(), Fail> {
    let bad: Vec<String> = verdicts(s)
        .into_iter()
        .filter(|(_, v)| *v == Verdict::Stale)
        .map(|(p, _)| {
            format!(
                "{} {:?} name={:?} src={:?}",
                p.path, p.app, p.name, p.source
            )
        })
        .collect();
    need(bad.is_empty(), || format!("STALE presentation(s): {bad:?}"))
}

fn last_pres(s: &dyn Sut, path: &str) -> Option<(Presentation, Verdict)> {
    verdicts(s).into_iter().rev().find(|(p, _)| p.path == path)
}

/// Vacuity guard: the reader's mirror holds `path` with `name` right now.
fn mirror_has(s: &dyn Sut, app: &str, path: &str, name: &str) -> Result<(), Fail> {
    match s.probe(app, path) {
        Probe::Hit { name: n, .. } if n == name => Ok(()),
        other => Err(Fail(format!(
            "VACUOUS: expected {path} cached as {name:?} before the mutation, probe says {other:?}"
        ))),
    }
}

/// Vacuity guard with the node's current name read from the app.
fn cached(s: &mut dyn Sut, app: &str, path: &str) -> Result<(), Fail> {
    let n = name_of(s, app, path)?;
    mirror_has(s, app, path, &n)
}

fn evented_ok(s: &dyn Sut, path: &str, want_name: &str) -> Result<(), Fail> {
    let (p, v) = last_pres(s, path).ok_or_else(|| Fail(format!("nothing presented for {path}")))?;
    need(p.name == want_name, || {
        format!(
            "presented {:?}, wanted {want_name:?} ({:?})",
            p.name, p.source
        )
    })?;
    need(matches!(v, Verdict::Correct | Verdict::Refined), || {
        format!("verdict {v:?} for an evented change settled >= 50 ms before the focus")
    })
}

fn corrections_of(s: &dyn Sut) -> usize {
    verdict::corrections(&verdicts(s))
}

fn ok() -> CaseResult {
    Ok(Outcome::Pass)
}

// ---------------------------------------------------------------------------------------------

fn ic01(s: &mut dyn Sut) -> CaseResult {
    s.start_app("settings", "settings")?;
    let n = s.path_of("settings", "controls[3]")?;
    let m = s.path_of("settings", "controls[4]")?;
    let old = name_of(s, "settings", &n)?;
    s.focus("settings", &n)?;
    s.focus("settings", &m)?;
    mirror_has(s, "settings", &n, &old)?;
    s.must(
        "settings",
        Cmd::Rename {
            node: n.clone(),
            name: "Renamed evented".into(),
            evented: true,
        },
    )?;
    s.advance(100);
    s.focus("settings", &n)?;
    evented_ok(s, &n, "Renamed evented")?;
    no_stale(s)?;
    ok()
}

fn ic02(s: &mut dyn Sut) -> CaseResult {
    s.start_app("settings", "settings")?;
    // (a) G-1: the label announces, the control derived from it does not.
    let d = s.path_of("settings", "derived[0]")?;
    let label = s.path_of("settings", "derived_labels[0]")?;
    let other = s.path_of("settings", "controls[0]")?;
    let old = name_of(s, "settings", &d)?;
    s.focus("settings", &d)?;
    s.focus("settings", &other)?;
    mirror_has(s, "settings", &d, &old)?;
    s.must(
        "settings",
        Cmd::Rename {
            node: label,
            name: "Derived new".into(),
            evented: true,
        },
    )?;
    s.advance(100);
    s.focus("settings", &d)?;
    need(name_of(s, "settings", &d)? == "Derived new", || {
        "world did not derive the name".into()
    })?;
    // (b) silent rename of a plain node, then focus it.
    let n = s.path_of("settings", "controls[5]")?;
    let old_n = name_of(s, "settings", &n)?;
    s.focus("settings", &n)?;
    s.focus("settings", &other)?;
    mirror_has(s, "settings", &n, &old_n)?;
    s.must(
        "settings",
        Cmd::Rename {
            node: n.clone(),
            name: "Silent new".into(),
            evented: false,
        },
    )?;
    s.advance(100);
    s.focus("settings", &n)?;
    // (c) rename the focused node silently, move away and come back.
    s.must(
        "settings",
        Cmd::Rename {
            node: n.clone(),
            name: "Silent newer".into(),
            evented: false,
        },
    )?;
    s.advance(100);
    s.focus("settings", &other)?;
    s.focus("settings", &n)?;
    s.advance(300);
    no_stale(s)?;
    let (p, v) = last_pres(s, &n).ok_or_else(|| Fail("nothing presented".into()))?;
    need(v != Verdict::Stale, || format!("{p:?}"))?;
    need(corrections_of(s) >= 1, || {
        "silent renames must be caught by RV-1 and counted as corrections".into()
    })?;
    ok()
}

fn ic03(s: &mut dyn Sut) -> CaseResult {
    s.start_app("settings", "settings")?;
    let other = s.path_of("settings", "controls[0]")?;
    // S1 path: state flips on the focused node.
    for (i, st) in ["checked", "expanded", "selected", "sensitive"]
        .iter()
        .enumerate()
    {
        let node = format!("switches[{i}]");
        let p = s.path_of("settings", &node)?;
        s.focus("settings", &p)?;
        let before = s.presentations().len();
        let on = *st != "sensitive";
        s.must(
            "settings",
            Cmd::SetState {
                node: p.clone(),
                state: (*st).into(),
                on,
                evented: true,
            },
        )?;
        s.advance(60);
        let new: Vec<Presentation> = s.presentations().split_off(before);
        let sp = new
            .iter()
            .find(|x| x.path == p && x.source == Source::S1)
            .ok_or_else(|| Fail(format!("no S1 speech for {st} on {p}")))?;
        need(sp.t_ns - sp.stimulus_ns <= 5_000_000, || {
            format!("S1 took {} us", (sp.t_ns - sp.stimulus_ns) / 1000)
        })?;
    }
    // Non-focused node: flip, later focus is correct.
    let n = s.path_of("settings", "switches[5]")?;
    let old = name_of(s, "settings", &n)?;
    s.focus("settings", &n)?;
    s.focus("settings", &other)?;
    mirror_has(s, "settings", &n, &old)?;
    s.must(
        "settings",
        Cmd::SetState {
            node: n.clone(),
            state: "checked".into(),
            on: true,
            evented: true,
        },
    )?;
    s.advance(100);
    s.focus("settings", &n)?;
    let (_, v) = last_pres(s, &n).ok_or_else(|| Fail("nothing presented".into()))?;
    let fin = s
        .presented(&n)
        .pop()
        .ok_or_else(|| Fail("nothing presented".into()))?;
    need(
        fin.states & (1 << super::world::state::CHECKED) != 0,
        || "checked bit missing from the final utterance".into(),
    )?;
    need(matches!(v, Verdict::Correct | Verdict::Refined), || {
        format!("{v:?}")
    })?;
    no_stale(s)?;
    ok()
}

fn ic04(s: &mut dyn Sut) -> CaseResult {
    s.start_app("settings", "settings")?;
    let p = s.path_of("settings", "slider[0]")?;
    s.focus("settings", &p)?;
    cached(s, "settings", &p)?;
    let before = s.presentations().len();
    s.must(
        "settings",
        Cmd::SetValue {
            node: p.clone(),
            value: 75.0,
            evented: true,
        },
    )?;
    s.advance(60);
    let new: Vec<Presentation> = s.presentations().split_off(before);
    let sp = new
        .iter()
        .find(|x| x.path == p && x.source == Source::S1)
        .ok_or_else(|| Fail("no S1 speech for the value change".into()))?;
    need(sp.value.as_deref() == Some("75"), || {
        format!("spoke value {:?}", sp.value)
    })?;
    no_stale(s)?;
    ok()
}

fn ic05(s: &mut dyn Sut) -> CaseResult {
    for remove_events in [true, false] {
        let label = if remove_events {
            "settings"
        } else {
            "settings2"
        };
        s.start_app("settings", label)?;
        let mut old = Vec::new();
        for i in 0..3 {
            let p = s.path_of(label, &format!("controls[{i}]"))?;
            let n = name_of(s, label, &p)?;
            s.focus(label, &p)?;
            mirror_has(s, label, &p, &n)?;
            old.push(p);
        }
        let t_replace = s.now_ns();
        s.must(
            label,
            Cmd::ReplaceSubtree {
                root: "panel".into(),
                nodes: 300,
                remove_events,
                add_events: true,
            },
        )?;
        s.advance(100);
        let panel = s.path_of(label, "panel")?;
        let first_new = s.path_of(label, "controls[0]")?;
        need(!old.contains(&first_new), || {
            "panel was not replaced".into()
        })?;
        s.focus(label, &first_new)?;
        let (p, v) =
            last_pres(s, &first_new).ok_or_else(|| Fail("new node not presented".into()))?;
        need(p.name == name_of(s, label, &first_new)?, || {
            format!("new node spoken as {:?}", p.name)
        })?;
        need(matches!(v, Verdict::Correct | Verdict::Refined), || {
            format!("{v:?}")
        })?;
        for o in &old {
            let late = s
                .presentations()
                .into_iter()
                .any(|x| &x.path == o && x.t_ns > t_replace);
            need(!late, || {
                format!("old node {o} presented after the panel was replaced")
            })?;
        }
        if s.supports(Feature::Probe) && remove_events {
            // The panel's child list changed: if the panel is cached its CHILDREN are stale.
            if let Probe::Hit { stale, .. } = s.probe(label, &panel) {
                need(stale, || {
                    "cached panel not marked stale after children-changed".into()
                })?;
            }
        }
    }
    no_stale(s)?;
    ok()
}

fn ic06(s: &mut dyn Sut) -> CaseResult {
    s.start_app("settings", "settings")?;
    let p = s.path_of("settings", "controls[1]")?;
    let other = s.path_of("settings", "controls[0]")?;
    let name = name_of(s, "settings", &p)?;
    s.focus("settings", &p)?;
    s.focus("settings", &other)?;
    mirror_has(s, "settings", &p, &name)?;
    let old_bus = s.bus_name("settings");
    let t_kill = s.now_ns();
    s.restart_app("settings")?;
    s.advance(100);
    let new_bus = s.bus_name("settings");
    need(new_bus != old_bus, || "restart kept the bus name".into())?;
    need(s.path_of("settings", "controls[1]")? == p, || {
        "restart did not reuse the paths (the case needs path reuse)".into()
    })?;
    // MA-2 at the app level: the old instance's mirror is released.
    need(s.mirror_nodes(&old_bus).is_none(), || {
        format!(
            "old instance mirror still holds {:?} nodes",
            s.mirror_nodes(&old_bus)
        )
    })?;
    s.focus("settings", &p)?;
    let after: Vec<Presentation> = s
        .presentations()
        .into_iter()
        .filter(|x| x.t_ns > t_kill)
        .collect();
    need(after.iter().all(|x| x.app_bus != old_bus), || {
        "presented from the old instance".into()
    })?;
    let first = after
        .iter()
        .find(|x| x.path == p)
        .ok_or_else(|| Fail("nothing presented after the restart".into()))?;
    need(
        first.source != Source::Hit && first.app_bus == new_bus,
        || format!("served from the old instance's cache: {first:?}"),
    )?;
    no_stale(s)?;
    ok()
}

fn ic07(s: &mut dyn Sut) -> CaseResult {
    s.start_app("settings", "settings")?;
    // (a) defunct on a non-focused cached node.
    let n = s.path_of("settings", "controls[2]")?;
    let m = s.path_of("settings", "controls[0]")?;
    s.focus("settings", &n)?;
    s.focus("settings", &m)?;
    cached(s, "settings", &n)?;
    s.must(
        "settings",
        Cmd::Destroy {
            node: n.clone(),
            events: DestroyEvents::DefunctOnly,
        },
    )?;
    s.advance(100);
    need(s.probe("settings", &n) == Probe::Gone, || {
        format!("{:?}", s.probe("settings", &n))
    })?;
    // (b) defunct on the focused node: nothing more is spoken from it.
    let f = s.path_of("settings", "controls[6]")?;
    s.focus("settings", &f)?;
    let t = s.now_ns();
    s.must(
        "settings",
        Cmd::Destroy {
            node: f.clone(),
            events: DestroyEvents::DefunctOnly,
        },
    )?;
    s.advance(300);
    need(
        !s.presentations().iter().any(|x| x.path == f && x.t_ns > t),
        || "spoke from a defunct focused node".into(),
    )?;
    need(s.probe("settings", &f) == Probe::Gone, || {
        "focused defunct node not tombstoned".into()
    })?;
    // (c) UnknownObject: the lease expires without any event.
    let l = s.path_of("settings", "controls[8]")?;
    s.focus("settings", &l)?;
    s.focus("settings", &m)?;
    s.must("settings", Cmd::ExpireLease { node: l.clone() })?;
    s.focus("settings", &l)?; // spoken from cache (may_refine), RV-1 learns the truth
    need(s.probe("settings", &l) == Probe::Gone, || {
        format!(
            "UnknownObject did not tombstone: {:?}",
            s.probe("settings", &l)
        )
    })?;
    let n_before = s.presentations().len();
    s.must("settings", Cmd::Focus { node: m.clone() })?;
    s.advance(100);
    s.advance(100);
    let new: Vec<Presentation> = s.presentations().split_off(n_before);
    need(!new.iter().any(|x| x.path == l), || {
        "spoke from the dead key after UnknownObject".into()
    })?;
    no_stale(s)?;
    ok()
}

fn ic08(s: &mut dyn Sut) -> CaseResult {
    s.start_app("settings", "settings")?;
    let list = s.path_of("settings", "sidebar_list")?;
    let other = s.path_of("settings", "controls[0]")?;
    for (i, mode) in [
        ReorderMode::RemoveAdd,
        ReorderMode::ChildrenOnly,
        ReorderMode::Silent,
    ]
    .into_iter()
    .enumerate()
    {
        let p = s.path_of("settings", &format!("sidebar[{}]", 3 + i))?;
        let name = name_of(s, "settings", &p)?;
        s.focus("settings", &p)?;
        s.focus("settings", &other)?;
        mirror_has(s, "settings", &p, &name)?;
        s.must(
            "settings",
            Cmd::Reorder {
                parent: list.clone(),
                mode,
            },
        )?;
        s.advance(100);
        s.focus("settings", &p)?;
        let (pr, v) = last_pres(s, &p)
            .filter(|(x, _)| x.t_ns > 0)
            .ok_or_else(|| Fail(format!("{mode:?}: reordered child not spoken")))?;
        let n_now = s.presented(&p).len();
        need(n_now >= 2, || {
            format!("{mode:?}: reordered child not presented again")
        })?;
        need(pr.name == name, || format!("{mode:?}: spoke {:?}", pr.name))?;
        need(v != Verdict::Stale, || format!("{mode:?}: {v:?}"))?;
        need(!matches!(s.probe("settings", &p), Probe::Gone), || {
            format!("{mode:?}: child stayed tombstoned (LR-3)")
        })?;
    }
    no_stale(s)?;
    ok()
}

fn ic09(s: &mut dyn Sut) -> CaseResult {
    for evented in [false, true] {
        let label = if evented { "files-ev" } else { "files" };
        s.start_app_with("files", label, &|m: &mut ModelSpec| {
            m.quirks.recycle_evented = evented;
        })?;
        let row = s.path_of(label, "rows[0]")?;
        let other = s.path_of(label, "rows[3]")?;
        let old = name_of(s, label, &row)?;
        s.focus(label, &row)?;
        s.focus(label, &other)?;
        mirror_has(s, label, &row, &old)?;
        s.must(label, Cmd::Scroll { steps: 7 })?;
        s.advance(100);
        s.focus(label, &row)?;
        let (p, v) = last_pres(s, &row).ok_or_else(|| Fail("row not presented".into()))?;
        need(v != Verdict::Stale, || format!("recycled row: {p:?}"))?;
        need(name_of(s, label, &row)? != old, || {
            "row did not change item".into()
        })?;
    }
    no_stale(s)?;
    ok()
}

fn ic10(s: &mut dyn Sut) -> CaseResult {
    if !s.supports(Feature::VirtualTime) {
        return skip("needs exact timing");
    }
    s.start_app("settings", "settings")?;
    // LR-1: a reply issued before the removal and delivered after it.
    let n = s.path_of("settings", "controls[4]")?;
    s.must(
        "settings",
        Cmd::DelayReplies {
            member: String::new(),
            ms: 100,
            count: 1,
            conn: None,
        },
    )?;
    s.must("settings", Cmd::Focus { node: n.clone() })?;
    s.advance(10);
    s.must(
        "settings",
        Cmd::Destroy {
            node: n.clone(),
            events: DestroyEvents::Full,
        },
    )?;
    s.advance(300);
    need(
        !matches!(s.probe("settings", &n), Probe::Hit { .. }),
        || {
            format!(
                "ghost: a late reply revived a removed key: {:?}",
                s.probe("settings", &n)
            )
        },
    )?;
    // LR-3: a request issued after the removal that succeeds is proof of life.
    let list = s.path_of("settings", "sidebar_list")?;
    let p = s.path_of("settings", "sidebar[7]")?;
    let other = s.path_of("settings", "controls[0]")?;
    s.focus("settings", &p)?;
    s.focus("settings", &other)?;
    s.must(
        "settings",
        Cmd::Reorder {
            parent: list,
            mode: ReorderMode::RemoveAdd,
        },
    )?;
    s.advance(100);
    s.focus("settings", &p)?;
    need(matches!(s.probe("settings", &p), Probe::Hit { .. }), || {
        "no revival after a proof-of-life reply".into()
    })?;
    no_stale(s)?;
    ok()
}

fn ic11(s: &mut dyn Sut) -> CaseResult {
    if !s.supports(Feature::LatencyKnobs) {
        return skip("needs per-direction latency control");
    }
    // (event_us, reply_us, extra reply delay ms, label)
    let variants = [
        (100, 5_000, 0, "event-before-reply"),
        (5_000, 100, 0, "reply-before-event"),
        (100, 100, 20, "pre-mutation reply delayed"),
    ];
    for (i, (ev, rep, delay, what)) in variants.into_iter().enumerate() {
        let label = format!("settings{i}");
        s.start_app("settings", &label)?;
        s.set_latency(ev, rep);
        let n = s.path_of(&label, "controls[2]")?;
        let old = name_of(s, &label, &n)?;
        if delay > 0 {
            s.must(
                &label,
                Cmd::DelayReplies {
                    member: String::new(),
                    ms: delay,
                    count: 1,
                    conn: None,
                },
            )?;
        }
        s.must(&label, Cmd::Focus { node: n.clone() })?;
        s.advance(1);
        s.must(
            &label,
            Cmd::Rename {
                node: n.clone(),
                name: "Raced".into(),
                evented: true,
            },
        )?;
        s.advance(300);
        let first = s
            .presented(&n)
            .into_iter()
            .next()
            .ok_or_else(|| Fail(format!("{what}: nothing presented")))?;
        need(first.name == old, || {
            format!(
                "{what}: the race did not happen (first utterance {:?}, old {old:?})",
                first.name
            )
        })?;
        match s.probe(&label, &n) {
            Probe::Hit { name, stale, .. } => need(stale || name == "Raced", || {
                format!("{what}: mirror is Fresh with the old value {name:?}")
            })?,
            Probe::Gone => return Err(Fail(format!("{what}: node tombstoned"))),
            Probe::Miss => {}
        }
        s.set_latency(100, 100);
    }
    no_stale(s)?;
    ok()
}

fn ic12(s: &mut dyn Sut) -> CaseResult {
    if !s.supports(Feature::CacheFill) {
        return skip("needs a reader that fills from GetItems/prefetch");
    }
    s.start_app("settings", "settings")?;
    let n = s.path_of("settings", "controls[3]")?;
    s.must("settings", Cmd::Hide { node: n.clone() })?;
    s.must(
        "settings",
        Cmd::Destroy {
            node: n.clone(),
            events: DestroyEvents::Silent,
        },
    )?;
    s.advance(35_000);
    need(
        !matches!(s.probe("settings", &n), Probe::Hit { stale: false, .. }),
        || "a destroyed hidden ghost is still live in the mirror".into(),
    )?;
    no_stale(s)?;
    ok()
}

fn ic13(s: &mut dyn Sut) -> CaseResult {
    s.start_app("settings", "settings")?;
    let r = s.must(
        "settings",
        Cmd::Add {
            parent: "panel".into(),
            name: "Late arrival".into(),
            during_dump: true,
            silent: false,
        },
    )?;
    let path = r.str("path").unwrap_or_default().to_owned();
    s.advance(100);
    s.focus("settings", &path)?;
    let (p, v) = last_pres(s, &path)
        .ok_or_else(|| Fail("node added during a dump cannot be focused".into()))?;
    need(p.name == "Late arrival" && v != Verdict::Stale, || {
        format!("{p:?} {v:?}")
    })?;
    ok()
}

fn ic14(s: &mut dyn Sut) -> CaseResult {
    s.start_app("shell", "shell")?;
    s.must("shell", Cmd::RebuildSwitcher { items: 4 })?;
    s.advance(100);
    let a0 = s.path_of("shell", "switcher[0]")?;
    let a1 = s.path_of("shell", "switcher[1]")?;
    for p in [&a0, &a1] {
        s.must("shell", Cmd::Select { node: p.clone() })?;
        s.advance(100);
        let (pr, v) =
            last_pres(s, p).ok_or_else(|| Fail(format!("selection of {p} not spoken")))?;
        need(
            v != Verdict::Stale && pr.name == name_of(s, "shell", p)?,
            || format!("{pr:?}"),
        )?;
    }
    let calls_before = s.calls("shell")?.total_calls();
    // Rebuild per Alt+Tab: new paths. Nothing may be fetched for the dead ones.
    s.must("shell", Cmd::RebuildSwitcher { items: 4 })?;
    s.advance(2_000);
    need(s.calls("shell")?.total_calls() == calls_before, || {
        "the reader fetched after a switcher rebuild nobody asked about".into()
    })?;
    need(s.calls("shell")?.calls_unknown_object == 0, || {
        "requests went to dead switcher paths".into()
    })?;
    let b0 = s.path_of("shell", "switcher[0]")?;
    need(b0 != a0, || "rebuild did not mint new paths".into())?;
    s.must("shell", Cmd::Select { node: b0.clone() })?;
    s.advance(100);
    need(last_pres(s, &b0).is_some(), || {
        "selection after rebuild not spoken".into()
    })?;
    no_stale(s)?;
    ok()
}

fn ic15(s: &mut dyn Sut) -> CaseResult {
    s.start_app("settings", "settings")?;
    let c = s.path_of("settings", "controls[5]")?;
    s.focus("settings", &c)?;
    let win = s.path_of("settings", "window")?;
    cached(s, "settings", &c)?;
    let t = s.now_ns();
    s.must("settings", Cmd::WindowDestroy { node: win.clone() })?;
    s.advance(300);
    need(s.probe("settings", &win) != Probe::Miss, || {
        "destroyed window forgotten, not tombstoned".into()
    })?;
    need(
        !matches!(s.probe("settings", &win), Probe::Hit { .. }),
        || "destroyed window still live".into(),
    )?;
    need(
        !s.presentations().iter().any(|x| x.path == c && x.t_ns > t),
        || "a descendant of the destroyed window was presented".into(),
    )?;
    ok()
}

fn ic16(s: &mut dyn Sut) -> CaseResult {
    if !s.supports(Feature::Malformed) {
        return skip("needs the malformed-ref mode of the fake app");
    }
    s.start_app("settings", "settings")?;
    s.start_app("storm", "other")?;
    let other_bus = s.bus_name("other");
    let c = s.path_of("settings", "controls[1]")?;
    s.must(
        "settings",
        Cmd::Malform {
            on: true,
            foreign_bus: other_bus,
        },
    )?;
    s.focus("settings", &c)?;
    if s.supports(Feature::WhereAmI) {
        s.where_am_i()?;
    }
    s.advance(300);
    let calls = s.calls("other")?;
    need(calls.total_calls() == 0, || {
        format!(
            "the reader followed a foreign ref into another app: {} calls",
            calls.total_calls()
        )
    })?;
    no_stale(s)?;
    ok()
}

fn ic17(s: &mut dyn Sut) -> CaseResult {
    s.start_app("settings", "settings")?;
    s.start_app("storm", "storm")?;
    let term = s.path_of("storm", "terminal")?;
    let win = s.path_of("storm", "window")?;
    s.focus("storm", &term)?;
    s.focus("storm", &win)?;
    mirror_has(s, "storm", &term, "terminal")?;
    let c = s.path_of("settings", "controls[1]")?;
    s.focus("settings", &c)?;
    let before = s.calls("storm")?.total_calls();
    s.must(
        "storm",
        Cmd::Storm {
            rate_per_s: 50_000,
            duration_ms: 200,
        },
    )?;
    s.advance(400);
    // Dirty nodes are stale, not dropped.
    match s.probe("storm", &win) {
        Probe::Hit { stale, .. } => {
            need(stale, || "flood did not mark the app's nodes dirty".into())?
        }
        other => {
            return Err(Fail(format!(
                "dirty node dropped instead of stale: {other:?}"
            )));
        }
    }
    let delta = s.calls("storm")?.total_calls() - before;
    need(delta <= 100, || {
        format!("{delta} calls to the flooded app in 0.4 s (rate limit)")
    })?;
    // The focused app is unaffected.
    match s.probe("settings", &c) {
        Probe::Hit { stale, .. } => need(!stale, || {
            "another app's flood staled the focused app".into()
        })?,
        other => return Err(Fail(format!("focused app's node lost: {other:?}"))),
    }
    no_stale(s)?;
    ok()
}

fn ic18(s: &mut dyn Sut) -> CaseResult {
    s.start_app("settings", "settings")?;
    let n = s.path_of("settings", "controls[2]")?;
    let m = s.path_of("settings", "controls[0]")?;
    s.focus("settings", &n)?;
    s.focus("settings", &m)?;
    cached(s, "settings", &n)?;
    let t_hang = s.now_ns();
    s.must(
        "settings",
        Cmd::Hang {
            mode: HangMode::Never,
            ms: 0,
        },
    )?;
    // Three timeouts make the app Hung (the deadline of an Interactive request is 120 ms).
    for _ in 0..3 {
        s.must("settings", Cmd::Focus { node: n.clone() })?;
        s.advance(150);
        s.must("settings", Cmd::Focus { node: m.clone() })?;
        s.advance(150);
    }
    let from_cache: Vec<Presentation> = s
        .presented(&n)
        .into_iter()
        .filter(|p| p.t_ns > t_hang)
        .collect();
    need(!from_cache.is_empty(), || {
        "nothing presented from the cache for a hung app".into()
    })?;
    need(from_cache.iter().all(|p| p.may_refine), || {
        "a presentation from cache while the app hangs must be tagged may_refine".into()
    })?;
    let before = s.calls("settings")?.total_calls();
    for _ in 0..5 {
        s.must("settings", Cmd::Focus { node: n.clone() })?;
        s.advance(500);
        s.must("settings", Cmd::Focus { node: m.clone() })?;
        s.advance(500);
    }
    let sent = s.calls("settings")?.total_calls() - before;
    need(sent <= 6, || {
        format!("{sent} calls to a Hung app in 5 s (only 1/s probes allowed)")
    })?;
    // Recovery.
    s.must(
        "settings",
        Cmd::Hang {
            mode: HangMode::Off,
            ms: 0,
        },
    )?;
    s.advance(2_000);
    let r = s.presentations().len();
    s.focus("settings", &n)?;
    need(s.presentations().len() > r, || {
        "no recovery after the app answers again".into()
    })?;
    ok()
}

fn ic19(s: &mut dyn Sut) -> CaseResult {
    for (mode, label) in [
        (P2pMode::Peer, "p-peer"),
        (P2pMode::SameBus, "p-same"),
        (P2pMode::None, "p-none"),
    ] {
        s.start_app_with("settings", label, &|m: &mut ModelSpec| m.p2p = mode)?;
        let c = s.path_of(label, "controls[1]")?;
        s.focus(label, &c)?;
        let (p, v) =
            last_pres(s, &c).ok_or_else(|| Fail(format!("{mode:?}: nothing presented")))?;
        need(v != Verdict::Stale, || format!("{mode:?}: {p:?}"))?;
        if mode == P2pMode::SameBus {
            need(s.calls(label)?.bus_callers <= 1, || {
                "SameBus opened a duplicate connection instead of reusing the call connection"
                    .into()
            })?;
        }
        if mode == P2pMode::Peer {
            // EOF mid-request, then a connect hang: the reader must fall back and still speak.
            s.must(label, Cmd::ClosePeers)?;
            let d = s.path_of(label, "controls[2]")?;
            s.focus(label, &d)?;
            need(last_pres(s, &d).is_some(), || {
                "no presentation after the peer connection died".into()
            })?;
            s.must(label, Cmd::PeerAcceptDelay { ms: 3_000 })?;
            let e = s.path_of(label, "controls[3]")?;
            s.must(label, Cmd::Focus { node: e.clone() })?;
            s.advance(5_000);
            need(last_pres(s, &e).is_some(), || {
                "no presentation within Startup while the peer hangs".into()
            })?;
        }
    }
    no_stale(s)?;
    ok()
}

fn ic20(s: &mut dyn Sut) -> CaseResult {
    s.start_app("ff-chrome", "ff")?;
    let doc = s.path_of("ff", "document")?;
    let chrome = s.path_of("ff", "window")?;
    s.focus("ff", &chrome)?;
    s.focus("ff", &doc)?;
    s.advance(2_000);
    let st = s.calls("ff")?;
    need(st.calls_below_document == 0, || {
        format!(
            "{} fill request(s) below the document node",
            st.calls_below_document
        )
    })?;
    ok()
}

fn ic21(s: &mut dyn Sut) -> CaseResult {
    s.start_app("settings", "settings")?;
    let n = s.path_of("settings", "controls[2]")?;
    let m = s.path_of("settings", "controls[0]")?;
    // Before the reader's first request the gated app announces nothing.
    s.must(
        "settings",
        Cmd::Rename {
            node: n.clone(),
            name: "Before registration".into(),
            evented: true,
        },
    )?;
    s.advance(100);
    s.focus("settings", &n)?;
    let (p, v) = last_pres(s, &n).ok_or_else(|| Fail("nothing presented".into()))?;
    need(
        p.name == "Before registration" && v != Verdict::Stale,
        || format!("presented pre-registration state: {p:?}"),
    )?;
    s.focus("settings", &m)?;
    mirror_has(s, "settings", &n, "Before registration")?;
    // Registered now: the same mutation is announced and invalidates.
    s.must(
        "settings",
        Cmd::Rename {
            node: n.clone(),
            name: "After registration".into(),
            evented: true,
        },
    )?;
    s.advance(100);
    match s.probe("settings", &n) {
        Probe::Hit { stale, name, .. } => need(stale || name == "After registration", || {
            "event after registration was not applied".into()
        })?,
        other => return Err(Fail(format!("{other:?}"))),
    }
    s.focus("settings", &n)?;
    evented_ok(s, &n, "After registration")?;
    no_stale(s)?;
    ok()
}

fn ic22(s: &mut dyn Sut) -> CaseResult {
    if !s.supports(Feature::CacheFill) {
        return skip("needs a reader that can send CacheDump");
    }
    // (a) a self-declared Shell that does not own org.gnome.Shell.
    s.start_app_with("shell", "fake-shell", &|m: &mut ModelSpec| {
        m.owns_name = None;
        m.app_name = "GNOME Shell".into();
        m.toolkit = "gnome-shell".into();
    })?;
    let c = s.path_of("fake-shell", "clock")?;
    s.focus("fake-shell", &c)?;
    s.advance(3_000);
    need(s.calls("fake-shell")?.member_calls("GetItems") == 0, || {
        "CacheDump sent to an app that only claims to be gnome-shell".into()
    })?;
    // (b) the verified Shell whose dump is implausible.
    s.start_app_with("shell", "real-shell", &|m: &mut ModelSpec| {
        m.owns_name = Some("org.gnome.Shell".into());
        m.quirks.dump_realised_only = true;
    })?;
    let c2 = s.path_of("real-shell", "clock")?;
    s.focus("real-shell", &c2)?;
    s.advance(40_000);
    need(s.calls("real-shell")?.member_calls("GetItems") <= 1, || {
        "a 2-item dump did not clear the eligibility flag".into()
    })?;
    no_stale(s)?;
    ok()
}

fn ic23(s: &mut dyn Sut) -> CaseResult {
    if !s.supports(Feature::WhereAmI) {
        return skip("needs the where-am-I command");
    }
    s.start_app("settings", "settings")?;
    let c = s.path_of("settings", "controls[3]")?;
    let p = parent_of(s, "settings", &c)?;
    let old = name_of(s, "settings", &p)?;
    s.focus("settings", &p)?;
    s.focus("settings", &c)?;
    mirror_has(s, "settings", &p, &old)?;
    let calls = s.calls("settings")?.total_calls();
    s.advance(31_000);
    need(s.calls("settings")?.total_calls() == calls, || {
        "TTL caused calls while idle".into()
    })?;
    s.must(
        "settings",
        Cmd::Rename {
            node: p.clone(),
            name: "Parent renamed".into(),
            evented: false,
        },
    )?;
    let before = s.presentations().len();
    s.where_am_i()?;
    s.advance(500);
    let new: Vec<Presentation> = s.presentations().split_off(before);
    let spoke = new
        .iter()
        .find(|x| x.path == p && x.source == Source::Hit)
        .ok_or_else(|| Fail("the aged ancestor was not spoken as context".into()))?;
    need(spoke.may_refine, || {
        "aged node spoken without may_refine".into()
    })?;
    let fixed = new
        .iter()
        .any(|x| x.path == p && x.refines.is_some() && x.name == "Parent renamed");
    need(fixed, || {
        "the aged ancestor was not revalidated after being spoken".into()
    })?;
    no_stale(s)?;
    ok()
}

pub fn all_cases() -> Vec<Case> {
    macro_rules! c {
        ($id:literal, $title:literal, $f:ident) => {
            Case {
                id: $id,
                title: $title,
                needs_probe: false,
                run: $f,
            }
        };
        ($id:literal, $title:literal, $f:ident, probe) => {
            Case {
                id: $id,
                title: $title,
                needs_probe: true,
                run: $f,
            }
        };
    }
    vec![
        c!("IC-1", "rename, evented", ic01, probe),
        c!("IC-2", "rename, silent (G-1, S-1)", ic02, probe),
        c!("IC-3", "state flip", ic03, probe),
        c!("IC-4", "value", ic04, probe),
        c!("IC-5", "subtree replace", ic05, probe),
        c!("IC-6", "app restart", ic06, probe),
        c!("IC-7", "defunct and UnknownObject", ic07, probe),
        c!("IC-8", "reorder", ic08, probe),
        c!("IC-9", "recycled rows", ic09, probe),
        c!("IC-10", "late reply", ic10, probe),
        c!("IC-11", "reply/event race", ic11, probe),
        c!("IC-12", "hidden ghost", ic12),
        c!("IC-13", "add during dump", ic13),
        c!("IC-14", "Shell rebuilds", ic14),
        c!("IC-15", "window destroy", ic15, probe),
        c!("IC-16", "foreign refs", ic16),
        c!("IC-17", "flood", ic17, probe),
        c!("IC-18", "hung app", ic18, probe),
        c!("IC-19", "P2P modes", ic19),
        c!("IC-20", "document boundary", ic20),
        c!("IC-21", "listener gate", ic21, probe),
        c!("IC-22", "dump eligibility", ic22),
        c!("IC-23", "TTL", ic23, probe),
    ]
}

/// Result of one case against a fresh SUT.
#[derive(Debug)]
pub struct CaseReport {
    pub id: &'static str,
    pub result: CaseResult,
}

/// Run every case, each against a fresh SUT from `make`.
pub fn run_suite(make: &dyn Fn() -> Box<dyn Sut>) -> Vec<CaseReport> {
    all_cases()
        .into_iter()
        .map(|c| {
            let mut sut = make();
            let result = if c.needs_probe && !sut.supports(Feature::Probe) {
                skip("needs the MirrorRead probe (SPEC 5.7)")
            } else {
                (c.run)(sut.as_mut())
            };
            CaseReport { id: c.id, result }
        })
        .collect()
}

/// Truth helper re-exported for the walk driver.
pub fn truth_db(s: &dyn Sut) -> TruthDb {
    TruthDb::new(&s.truth())
}
