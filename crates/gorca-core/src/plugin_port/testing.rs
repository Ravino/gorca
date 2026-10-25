// SPDX-License-Identifier: GPL-3.0-or-later
//! Test doubles of the plugin port: a plugin that answers at once with the text of its
//! children (shared by the document stage and app writer tests).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use gorca_model::sem::Group;

use super::{
    FactEffect, FactValue, Governor, Handoff, Job, PluginMeta, PluginService, PluginTier, Reply,
    ReplyBody, Row, ScopeRunner, ScopeSpec,
};

/// A plugin that answers at once: the name of a watched row is "<text of its children>!".
pub(crate) struct Echo {
    metas: Vec<PluginMeta>,
    queue: Arc<Mutex<Vec<Reply>>>,
    jobs: Arc<Mutex<u32>>,
}

impl Echo {
    fn answer(&self, jobs: &[Job]) {
        for j in jobs {
            *self.jobs.lock().unwrap() += 1;
            for d in &j.nodes {
                let Some(r) = j.rows.rows.get(&d.node) else {
                    continue;
                };
                let mut reads = vec![d.node];
                let mut text = String::new();
                for c in &r.children {
                    if let Some(cr) = j.rows.rows.get(c) {
                        reads.push(*c);
                        if let Some(n) = &cr.row.name {
                            text.push_str(n);
                        }
                    }
                }
                self.queue.lock().unwrap().push(Reply {
                    plugin: j.plugin,
                    t: j.t,
                    targets: vec![d.node],
                    body: ReplyBody::Done {
                        effects: vec![FactEffect {
                            node: d.node,
                            group: Group::Name.group_code().0,
                            value: Some(FactValue::Text(Arc::from(format!("{text}!")))),
                            patch: true,
                        }],
                        reads,
                        overflow: false,
                    },
                });
            }
        }
    }
}

impl ScopeRunner for Echo {
    fn plugins(&self) -> &[PluginMeta] {
        &self.metas
    }
    fn watches(&self, row: &Row) -> Vec<(usize, u16)> {
        if row.class.as_deref() == Some("watch") {
            vec![(0, 1)]
        } else {
            Vec::new()
        }
    }
    fn interactive(&mut self, _t0: u64, _wait: bool, jobs: Vec<Job>) -> Handoff {
        self.answer(&jobs);
        Handoff::default()
    }
    fn background(&mut self, jobs: Vec<Job>) {
        self.answer(&jobs);
    }
    fn poll(&mut self, out: &mut Vec<Reply>) {
        out.append(&mut self.queue.lock().unwrap());
    }
    fn outstanding(&self) -> bool {
        false
    }
}

#[derive(Debug)]
pub(crate) struct Svc {
    jobs: Arc<Mutex<u32>>,
    gov: Arc<Governor>,
    wants_url: bool,
    /// Refuse an unverified scope (what the daemon's service does for URL-scoped plugins).
    require_verified: bool,
}

impl PluginService for Svc {
    fn wants_url(&self) -> bool {
        self.wants_url
    }
    fn open(
        &self,
        scope: &ScopeSpec,
        _wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Option<Box<dyn ScopeRunner>> {
        if self.require_verified && !scope.verified {
            return None;
        }
        Some(Box::new(Echo {
            metas: vec![PluginMeta {
                name: Arc::from("gorca.echo"),
                tier: PluginTier::FirstParty,
            }],
            queue: Arc::new(Mutex::new(Vec::new())),
            jobs: Arc::clone(&self.jobs),
        }))
    }
    fn governor(&self) -> Arc<Governor> {
        Arc::clone(&self.gov)
    }
}

pub(crate) fn svc(wants_url: bool) -> (Arc<dyn PluginService>, Arc<Mutex<u32>>) {
    svc_with(wants_url, false)
}

pub(crate) fn svc_with(
    wants_url: bool,
    require_verified: bool,
) -> (Arc<dyn PluginService>, Arc<Mutex<u32>>) {
    let jobs = Arc::new(Mutex::new(0));
    (
        Arc::new(Svc {
            jobs: Arc::clone(&jobs),
            gov: Arc::new(Governor::default()),
            wants_url,
            require_verified,
        }),
        jobs,
    )
}
