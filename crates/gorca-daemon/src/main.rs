//! `gorca` binary entry point. See the library crate for what is wired.

use std::process::ExitCode;
use std::time::Duration;

use anyhow::Result;
use gorca_daemon::{Parsed, USAGE};
use gorca_rt::CancellationToken;

fn main() -> Result<ExitCode> {
    let opts = match gorca_daemon::parse(std::env::args().skip(1)) {
        Ok(Parsed::Run(opts)) => opts.with_env(|k| std::env::var(k).ok()),
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
    let runtime = gorca_daemon::build_runtime()?;
    let result = runtime.block_on(gorca_daemon::run(opts, CancellationToken::new()));
    // Do not wait forever for a stuck blocking task: the process must exit.
    runtime.shutdown_timeout(Duration::from_secs(2));
    result.map(|()| ExitCode::SUCCESS)
}
