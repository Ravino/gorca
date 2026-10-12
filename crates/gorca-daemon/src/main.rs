//! `gorca` binary entry point. See the library crate for what Phase 0 does.

use std::time::Duration;

use anyhow::Result;
use gorca_rt::CancellationToken;

fn main() -> Result<()> {
    gorca_daemon::init_tracing()?;
    let runtime = gorca_daemon::build_runtime()?;
    let result = runtime.block_on(gorca_daemon::run(CancellationToken::new()));
    // Do not wait forever for a stuck blocking task: the process must exit.
    runtime.shutdown_timeout(Duration::from_secs(2));
    result
}
