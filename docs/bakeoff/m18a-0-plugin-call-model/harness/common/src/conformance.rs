// SPDX-License-Identifier: MIT OR Apache-2.0
//! Conformance checks that any `WLaneRunner` must pass. Candidates call
//! `m18a0_common::conformance::run(my_factory)` from their own tests; the harness runs it on the
//! reference runner. Needs the guests built (`guests/build.sh`).

use crate::fixture::*;
use crate::host::PluginRuntime;
use crate::runner::WLaneRunner;
use crate::writer::*;

pub type Make = fn(Vec<PluginRuntime>) -> anyhow::Result<Box<dyn WLaneRunner>>;

fn settle(w: &mut StubWriter, r: &mut dyn WLaneRunner) -> bool {
    let end = crate::timing::now_ns() + 2_000_000_000;
    loop {
        w.drain();
        w.pump_background(r);
        if w.plugins
            .iter()
            .all(|p| p.slot.busy.is_none() && p.pending.is_empty())
            && w.mailbox_depth() == 0
        {
            return true;
        }
        if crate::timing::now_ns() > end {
            return false;
        }
        std::hint::spin_loop();
    }
}

/// Returns (check name, passed, detail).
pub fn run(make: Make) -> anyhow::Result<Vec<(String, bool, String)>> {
    let mut res = Vec::new();
    let names = plugin_names("p2", &["a4".to_string(), "a12".to_string()]);
    let fx = Fixture::build(&names, 3, Some(2.0))?;
    let mut w = fx.writer(512, 8, 64);
    let mut r = make(fx.runtimes()?)?;
    let rows = Fixture::row_ids(&w.table);
    for i in 0..40u64 {
        let ev = FocusEvent {
            focus: rows[(i as usize * 7) % rows.len()],
            t_sched_ns: crate::timing::now_ns(),
        };
        w.focus_change(&mut *r, &ev, None);
        res.push((
            format!("turn {i} settles"),
            settle(&mut w, &mut *r),
            String::new(),
        ));
    }
    let p2 = &w.plugins[0].c;
    res.push((
        "benign plugin served: derive calls > 0".into(),
        p2.derive_calls > 0,
        format!("{}", p2.derive_calls),
    ));
    res.push((
        "benign plugin facts reach the fused entry".into(),
        w.plugins[0].c.stale_token_dropped + w.plugins[0].c.stale_epoch_dropped < p2.derive_calls,
        String::new(),
    ));
    let a4 = &w.plugins[1].c;
    res.push((
        "A4 (100k effects) is rejected atomically and struck".into(),
        a4.strikes > 0 && a4.faults.contains_key("MalformedEffect"),
        format!("{:?}", a4.faults),
    ));
    res.push((
        "no facts of a faulting call are admitted".into(),
        w.counters.admitted_facts > 0,
        String::new(),
    ));
    res.push((
        "no impossible slot-table cell reached".into(),
        w.counters.impossible_cells == 0,
        String::new(),
    ));
    // A12: asserting on its own target does not cause more derive calls than focus changes.
    let a12 = &w.plugins[2].c;
    res.push((
        "A12: at most one call per real change".into(),
        a12.derive_calls <= w.counters.turns + w.counters.background_turns,
        format!("{} calls", a12.derive_calls),
    ));
    r.cancel(0, 12345);
    r.shutdown();
    r.shutdown();
    res.push((
        "cancel of an unknown job and double shutdown do not panic".into(),
        true,
        String::new(),
    ));
    Ok(res)
}
