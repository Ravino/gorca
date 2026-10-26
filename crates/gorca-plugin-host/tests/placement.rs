// SPDX-License-Identifier: GPL-3.0-or-later
//! CPU placement of the worker threads (M18a-5): the mask a worker runs with follows the class of
//! its plugin, replacements of a wedged actor included, and no placement leaves the mask alone.
#![allow(
    clippy::pedantic,
    clippy::disallowed_methods,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use std::sync::{Arc, Mutex};

use gorca_plugin_host::actor::{ActorRunner, Config, WorkerPlacement};
use gorca_plugin_host::call::{DeriveJob, Exec, PluginReply};
use gorca_rt::sched;

/// Records the CPU mask of the thread that builds its instance (`warm` runs on the worker).
struct Probe(Arc<Mutex<Vec<usize>>>);

impl Exec for Probe {
    fn run(&mut self, job: &DeriveJob) -> PluginReply {
        PluginReply::Cancelled {
            plugin: job.plugin,
            job: job.job,
            slot_epoch: job.slot_epoch,
        }
    }
    fn reset(&mut self) {}
    fn warm(&mut self) {
        *self.0.lock().unwrap() = sched::allowed_cpus();
    }
}

fn masks(place: Option<WorkerPlacement>) -> (Vec<usize>, Vec<usize>) {
    let a = Arc::new(Mutex::new(Vec::new()));
    let b = Arc::new(Mutex::new(Vec::new()));
    let execs: Vec<(u32, Box<dyn Exec>)> = vec![
        (0, Box::new(Probe(Arc::clone(&a)))),
        (1, Box::new(Probe(Arc::clone(&b)))),
    ];
    let r = ActorRunner::new_placed(execs, Box::new(|_| None), Config::default(), place).unwrap();
    let out = (a.lock().unwrap().clone(), b.lock().unwrap().clone());
    drop(r);
    out
}

fn two_cpus() -> Option<(Vec<usize>, Vec<usize>, Vec<usize>)> {
    let all = sched::allowed_cpus();
    if all.len() < 3 {
        return None;
    }
    Some((all.clone(), vec![all[0]], vec![all[1]]))
}

#[test]
fn workers_follow_the_class_of_their_plugin() {
    let Some((_, fast, bulk)) = two_cpus() else {
        return;
    };
    let place = WorkerPlacement {
        cpus: vec![Some(fast.clone()), Some(bulk.clone())],
    };
    let (a, b) = masks(Some(place));
    assert_eq!(a, fast, "a waited-for plugin runs on the fast cores");
    assert_eq!(b, bulk, "a background plugin runs on the efficiency cores");
}

#[test]
fn no_efficiency_cores_leaves_background_workers_alone() {
    let Some((all, fast, _)) = two_cpus() else {
        return;
    };
    let place = WorkerPlacement {
        cpus: vec![Some(fast.clone()), None],
    };
    let (a, b) = masks(Some(place));
    assert_eq!(a, fast);
    assert_eq!(b, all);
}

#[test]
fn without_placement_nothing_is_pinned() {
    let all = sched::allowed_cpus();
    let (a, b) = masks(None);
    assert_eq!(a, all);
    assert_eq!(b, all);
}
