//! Size and cost probes for the fusion memory/latency trade-off (math report §11).
//! Run: CARGO_TARGET_DIR=/var/tmp/fuse-mem-target taskset -c 2 cargo run --release
use std::hint::black_box;
use std::mem::size_of;
use std::sync::Arc;
use std::time::Instant;

use gorca_fuse::*;
use gorca_model::sem::{Avail, Check, Expand, Group, SemEntry, SemRole};

fn regs(i: usize, rich: bool) -> NodeRegs {
    let mut r = NodeRegs::default();
    let s = r.src_mut(PrimarySlot::P0);
    s.apply_life(1, true);
    let roles = [SemRole::Generic, SemRole::Button, SemRole::Checkbox, SemRole::Link, SemRole::Heading];
    s.apply_obs(2, Write::Val(Val::Role(roles[i % 5])));
    s.apply_obs(3, Write::Val(Val::Name(Arc::from(format!("node {i}").as_str()))));
    s.apply_obs(4, Write::Val(Val::Avail(Avail::Enabled)));
    s.apply_obs(5, Write::Val(Val::Check(Check::Off)));
    if rich {
        // A full AT-SPI state read: the remaining state groups observed absent, plus expand.
        s.apply_obs(6, Write::Val(Val::Expand(Expand::Collapsed)));
        for (k, g) in [Group::Description, Group::Press, Group::Select, Group::Current, Group::Invalid,
                       Group::Popup, Group::Flags, Group::Level, Group::SetPos].into_iter().enumerate() {
            s.apply_obs(7 + k as u64, Write::Absent(g));
        }
    }
    r
}

fn pct(v: &mut [f64], p: f64) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[((v.len() as f64 - 1.0) * p) as usize]
}

fn main() {
    println!("size Reg {} Write {} Slot {} S0Slot {} SrcRegs {} NodeRegs {} SemEntry {}",
        size_of::<Reg>(), size_of::<Write>(), size_of::<Slot>(), size_of::<S0Slot>(),
        size_of::<SrcRegs>(), size_of::<NodeRegs>(), size_of::<SemEntry>());
    for rich in [false, true] {
        let r = regs(1, rich);
        let n = r.src(PrimarySlot::P0).regs().len();
        println!("rich={rich}: regs={n} (heap len*32 = {} B; Vec cap after one-by-one inserts not exposed)", n * 32);
    }
    let mut profile = Profile::new(&[PrimarySlot::P0, PrimarySlot::P1], Rules::aria());
    profile.default_role = SemRole::Generic;
    let none = Ranked::default();
    for rich in [false, true] {
        for n in [1usize, 100_000] {
            let world: Vec<NodeRegs> = (0..n).map(|i| regs(i, rich)).collect();
            // random visiting order (LCG) for the cold case
            let mut order: Vec<usize> = (0..200_000).map(|k| (k * 2654435761usize) % n).collect();
            if n == 1 { order.truncate(200_000); }
            let mut scratch = Scratch::default();
            let mut s0t = Vec::with_capacity(order.len());
            let mut fut = Vec::with_capacity(order.len());
            for &i in &order {
                let t0 = Instant::now();
                let s0 = s0_with(&profile, &world[i], none.view(), &NoQuirks, &mut NoTrace);
                let t1 = Instant::now();
                let f = s0.as_ref().map(|s| fuse_node_with(&profile, &world[i], s, &Neighbours::NONE,
                    none.view(), &NoPatches, &mut scratch, &mut NoTrace));
                let t2 = Instant::now();
                black_box(&f);
                s0t.push((t1 - t0).as_nanos() as f64);
                fut.push((t2 - t1).as_nanos() as f64);
            }
            println!("rich={rich} n={n}: s0_with p50 {:.0} p99 {:.0} ns | fuse_node_with p50 {:.0} p99 {:.0} ns",
                pct(&mut s0t, 0.5), pct(&mut s0t, 0.99), pct(&mut fut, 0.5), pct(&mut fut, 0.99));
        }
    }
}
