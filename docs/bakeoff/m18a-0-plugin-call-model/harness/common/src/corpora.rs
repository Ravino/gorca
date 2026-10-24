// SPDX-License-Identifier: MIT OR Apache-2.0
//! Correctness corpora (SPEC 4 M10): the MQ-2 replay with randomized delivery delay (0 false
//! accepts), the PF-6 arrival-order permutation test (one fused entry), both driven through the
//! real shared admission path (`StubWriter::admit`), and the PF-1 helper types.
//! Everything is a function of the seed.

use crate::budget::Grant;
use crate::host::PluginReply;
use crate::host::{DoneReply, FactEffect, Value};
use crate::model::*;
use crate::slot::SlotState;
use crate::writer::*;

fn spec(name: &str, tier: u8) -> PluginSpec {
    PluginSpec {
        name: name.into(),
        tier,
        watch_roles: 1 << ROLE_ROW,
        adversary: false,
    }
}

fn fake_writer(nodes: usize, seed: u64, tiers: &[u8]) -> StubWriter {
    let specs: Vec<PluginSpec> = tiers
        .iter()
        .enumerate()
        .map(|(i, t)| spec(&format!("p{i}"), *t))
        .collect();
    let mut w = StubWriter::new(NodeTable::build(nodes, seed), specs, WriterCfg::spec(2.0));
    for e in &mut w.plugins {
        e.slot.state = SlotState::Ready;
    }
    w
}

fn done(
    w: &StubWriter,
    plugin: u16,
    job: u64,
    token: Token,
    facts: Vec<FactEffect>,
) -> PluginReply {
    PluginReply::Done(DoneReply {
        plugin,
        job,
        slot_epoch: w.plugins[plugin as usize].slot.epoch,
        interactive: true,
        grant: Grant {
            fuel: 1000,
            ns: 500,
            capped: false,
        },
        token,
        facts,
        refreshes: vec![],
        exec_ns: 1000,
        queued_ns: 0,
        fuel_used: 100,
        wall_overrun: false,
        imports: 0,
        dropped: 0,
        forged: 0,
    })
}

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct Mq2Result {
    pub runs: u64,
    pub false_accepts: u64,
    pub false_rejects: u64,
    pub accepted: u64,
    pub stale_dropped: u64,
    pub scope_wide_runs: u64,
    pub bind_gen_runs: u64,
}

/// Replay: a plugin call snapshots at `t`, reads a random set R in random classes M, the world
/// mutates a random number of times while the reply is delayed, then the reply is delivered.
/// The oracle recomputes staleness from the mutation log alone. Admitted-but-stale = false accept.
pub fn mq2_replay(runs: u64, seed: u64) -> Mq2Result {
    let mut rng = Rng(seed ^ 0x4D51_32_5245_504C);
    let mut res = Mq2Result::default();
    for run in 0..runs {
        let big = rng.below(20) == 0;
        let n_nodes = if big { 700 } else { 96 };
        let mut w = fake_writer(n_nodes, seed.wrapping_add(run), &[1]);
        let t_snap = w.table.t;
        let mut tok = Token::new(t_snap);
        let rsize = if big { 300 } else { 1 + rng.below(12) as usize };
        let start = rng.below(n_nodes as u64) as usize;
        let mask: u8 = (1 + rng.below(63)) as u8;
        let mut reads: Vec<NodeId> = Vec::new();
        for i in 0..rsize {
            let n = if big {
                ((start + i * 2) % n_nodes) as NodeId
            } else {
                rng.below(n_nodes as u64) as NodeId
            };
            reads.push(n);
            for c in ALL_CLASSES {
                if mask & (1 << c as u8) != 0 {
                    tok.note_read(n, c);
                }
            }
        }
        let scope_wide = tok.scope_wide.is_some();
        if scope_wide {
            res.scope_wide_runs += 1;
        }
        let target = rng.below(n_nodes as u64) as NodeId;
        let use_bind = rng.below(4) == 0;
        if use_bind {
            res.bind_gen_runs += 1;
            tok.bind
                .push((target, w.table.nodes[target as usize].bind_gen));
        }
        // Mutations during the reply delay.
        let mut stale_oracle = false;
        let muts = rng.below(9);
        for _ in 0..muts {
            let n = rng.below(n_nodes as u64) as NodeId;
            if rng.below(12) == 0 {
                w.table.rebind(n);
                if use_bind && n == target {
                    stale_oracle = true;
                }
            } else {
                let c = ALL_CLASSES[rng.below(6) as usize];
                w.table.touch(n, c);
                let in_mask = mask & (1 << c as u8) != 0;
                let in_r = if scope_wide { true } else { reads.contains(&n) };
                if in_mask && in_r {
                    stale_oracle = true;
                }
            }
        }
        let fact = FactEffect {
            node: target,
            group: 100 + (run % 800) as u16,
            value: Value::Number(run as u32),
            conf: 900,
            inferred: true,
        };
        let g = fact.group;
        let job = make_job(w.turn() + 1, 0, false);
        let r = done(&w, 0, job, tok, vec![fact]);
        w.admit(r);
        let admitted = w.fuse(target).winners.iter().any(|x| x.group == g);
        if admitted {
            res.accepted += 1;
        }
        if w.plugins[0].c.stale_token_dropped > 0 {
            res.stale_dropped += 1;
        }
        if stale_oracle && admitted {
            res.false_accepts += 1;
        }
        if !stale_oracle && !admitted {
            res.false_rejects += 1;
        }
        res.runs += 1;
    }
    res
}

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct Pf6Result {
    pub runs: u64,
    pub order_dependent: u64,
    pub permutations: u64,
}

fn permutations(n: usize) -> Vec<Vec<usize>> {
    fn go(cur: &mut Vec<usize>, used: &mut Vec<bool>, n: usize, out: &mut Vec<Vec<usize>>) {
        if cur.len() == n {
            out.push(cur.clone());
            return;
        }
        for i in 0..n {
            if !used[i] {
                used[i] = true;
                cur.push(i);
                go(cur, used, n, out);
                cur.pop();
                used[i] = false;
            }
        }
    }
    let mut out = Vec::new();
    go(&mut Vec::new(), &mut vec![false; n], n, &mut out);
    out
}

/// PF-6: three plugins reply with overlapping (node, group) facts and mixed tiers; every arrival
/// order of the three replies must give the same fused entries.
pub fn pf6_permutations(runs: u64, seed: u64) -> Pf6Result {
    let mut rng = Rng(seed ^ 0x5046_36);
    let perms = permutations(3);
    let mut res = Pf6Result::default();
    for run in 0..runs {
        let tiers = [
            1 + rng.below(3) as u8,
            1 + rng.below(3) as u8,
            1 + rng.below(3) as u8,
        ];
        let nodes: Vec<NodeId> = (0..4).map(|_| 2 + rng.below(60) as NodeId).collect();
        // Fixed reply contents per plugin for this run.
        let mut contents: Vec<Vec<FactEffect>> = Vec::new();
        for p in 0..3u32 {
            let mut v = Vec::new();
            for _ in 0..1 + rng.below(4) {
                v.push(FactEffect {
                    node: nodes[rng.below(4) as usize],
                    group: 100 + rng.below(3) as u16,
                    value: Value::Number(
                        (run as u32).wrapping_mul(7) + p * 1000 + rng.below(50) as u32,
                    ),
                    conf: 900,
                    inferred: true,
                });
            }
            contents.push(v);
        }
        let mut digests = Vec::new();
        for perm in &perms {
            let mut w = fake_writer(64, seed, &tiers);
            let job = make_job(1, 0, false);
            for &p in perm {
                let r = done(
                    &w,
                    p as u16,
                    job,
                    Token::new(w.table.t),
                    contents[p].clone(),
                );
                w.admit(r);
            }
            digests.push(w.fused_digest(&nodes));
            res.permutations += 1;
        }
        if digests.iter().any(|d| *d != digests[0]) {
            res.order_dependent += 1;
        }
        res.runs += 1;
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mq2_has_no_false_accepts_and_is_not_trivially_rejecting() {
        let r = mq2_replay(2000, 11);
        assert_eq!(r.false_accepts, 0, "{r:?}");
        assert!(r.accepted > 100 && r.stale_dropped > 100, "{r:?}");
        assert!(r.scope_wide_runs > 0 && r.bind_gen_runs > 0);
        assert_eq!(r.false_rejects, 0, "{r:?}");
    }

    #[test]
    fn pf6_arrival_order_does_not_change_the_fused_entry() {
        let r = pf6_permutations(300, 5);
        assert_eq!(r.order_dependent, 0, "{r:?}");
        assert_eq!(r.permutations, 300 * 6);
    }
}
