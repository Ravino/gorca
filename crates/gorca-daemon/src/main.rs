// SPDX-License-Identifier: GPL-3.0-or-later
//! `gorca` binary entry point. See the library crate for what is wired.

use std::process::ExitCode;
use std::time::Duration;

use anyhow::Result;
use gorca_daemon::{Parsed, USAGE};
use gorca_rt::CancellationToken;

fn main() -> Result<ExitCode> {
    // First statement (ADR-0027 CD-1): detect the CPU and resolve the kernel table before any
    // argument or config parsing can run a kernel. The disable list comes from `--cpu-disable`
    // (a minimal argv scan that runs no kernel) or `GORCA_CPU_DISABLE`. Nothing is logged yet.
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let cpu_list =
        gorca_daemon::cpu_disable_arg(&argv).or_else(|| std::env::var("GORCA_CPU_DISABLE").ok());
    let cpu = gorca_cpu::init(cpu_list.as_deref());
    // `gorca --version [--verbose]`: the revision the binary was built from (a test harness
    // compares it with the tree it measures); verbose adds the CPU feature report.
    if argv.first().map(String::as_str) == Some("--version") {
        println!("gorca {}", env!("GORCA_GIT_REV"));
        if argv.iter().any(|a| a == "--verbose" || a == "-v") {
            print!("{}", gorca_cpu::active().describe());
        }
        return Ok(ExitCode::SUCCESS);
    }
    // Opt-in: segment text in the `debug.explain_last` log (default off, it can be private).
    if std::env::var_os("GORCA_EXPLAIN_TEXT").is_some_and(|v| v == "1") {
        gorca_core::set_explain_text(true);
    }
    let opts = match gorca_daemon::parse(std::env::args().skip(1)) {
        Ok(Parsed::Run(opts)) => opts
            .with_env(|k| std::env::var(k).ok())
            .with_default_ext_dir(&gorca_daemon::default_ext_dirs(), |k| std::env::var(k).ok()),
        Ok(Parsed::Config(cmd)) => {
            let default = gorca_daemon::Options::default()
                .with_env(|k| std::env::var(k).ok())
                .config_path;
            let (text, ok) = gorca_daemon::run_config_cmd(&cmd, default.as_deref());
            print!("{text}");
            return Ok(if ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            });
        }
        Ok(Parsed::Help) => {
            print!("{USAGE}");
            return Ok(ExitCode::SUCCESS);
        }
        Err(e) => {
            eprintln!("gorca: {e}\n\n{USAGE}");
            return Ok(ExitCode::from(2));
        }
    };
    gorca_daemon::init_tracing()?;
    if let Err(e) = cpu {
        // Cannot happen when init is first; kept visible rather than silent.
        tracing::info!(target: "gorca::cpu", "cpu: {e}");
    }
    gorca_cpu::log_report();
    // The environment is read here only: `GORCA_RTKIT_BUS_ADDRESS` points the rtkit client at a
    // bus (the harness replaces DBUS_SYSTEM_BUS_ADDRESS with nothing), else the system bus.
    let rtkit_bus = ["GORCA_RTKIT_BUS_ADDRESS", "DBUS_SYSTEM_BUS_ADDRESS"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|s| !s.is_empty()));
    let mut opts = opts;
    opts.topology = opts.topology.resolve(rtkit_bus.as_deref());
    let runtime = gorca_daemon::build_runtime_for(
        opts.topology,
        opts.topology_report.as_deref(),
        rtkit_bus.as_deref(),
    )?;
    let result = runtime.block_on(gorca_daemon::run(opts, CancellationToken::new()));
    gorca_daemon::finish_topology();
    // Do not wait forever for a stuck blocking task: the process must exit.
    runtime.shutdown_timeout(Duration::from_secs(2));
    result.map(|()| ExitCode::SUCCESS)
}
