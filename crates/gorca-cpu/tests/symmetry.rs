// SPDX-License-Identifier: GPL-3.0-or-later
//! Heterogeneous cores (Intel P/E/LP-E, big.LITTLE) must report the same instruction set: the
//! kernel table is process-wide and threads migrate. This pins a thread to every CPU the process
//! may run on and compares the ISA-describing CPUID words (ADR-0027, `detect` module docs).
#![allow(unsafe_code)]

use std::mem::MaybeUninit;

fn allowed_cpus() -> Vec<usize> {
    // SAFETY: `set` is a plain bit array that sched_getaffinity fills; CPU_ISSET reads it.
    unsafe {
        let mut set = MaybeUninit::<libc::cpu_set_t>::zeroed().assume_init();
        if libc::sched_getaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &raw mut set) != 0 {
            return Vec::new();
        }
        (0..libc::CPU_SETSIZE as usize)
            .filter(|&c| libc::CPU_ISSET(c, &set))
            .collect()
    }
}

fn pin(cpu: usize) -> bool {
    // SAFETY: zeroed cpu_set_t is valid; CPU_SET writes one bit; the call affects this thread.
    unsafe {
        let mut set = MaybeUninit::<libc::cpu_set_t>::zeroed().assume_init();
        libc::CPU_SET(cpu, &mut set);
        libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &raw const set) == 0
    }
}

#[test]
fn every_cpu_reports_the_same_isa() {
    let cpus = allowed_cpus();
    let mut seen = Vec::new();
    for cpu in cpus {
        let isa = std::thread::spawn(move || pin(cpu).then(gorca_cpu::raw_isa))
            .join()
            .expect("probe thread");
        if let Some(isa) = isa {
            seen.push((cpu, isa));
        }
    }
    let Some((first_cpu, first)) = seen.first() else {
        return;
    };
    for (cpu, isa) in &seen {
        assert_eq!(
            isa, first,
            "cpu {cpu} reports a different ISA than cpu {first_cpu}: set GORCA_CPU_DISABLE or pin gorca to one core class"
        );
    }
    eprintln!("{} cpus checked, all report one ISA", seen.len());
}
