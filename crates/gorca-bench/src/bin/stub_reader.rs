//! `gorca-stub-reader`: the daemon stand-in of the e2e harness. See `gorca_bench::e2e::stub`.
//!
//! Runs until stdin reaches EOF (the harness owns the pipe, so the stub dies with it) and
//! prints one JSON line `{"event":"ready",...}` once the drain, lane and keyboard client are up.

use std::io::Read;

use anyhow::Result;
use gorca_bench::e2e::stub::{Stub, config_from_env};

fn main() -> Result<()> {
    let cfg = config_from_env()?;
    let stub = Stub::start(&cfg)?;
    let keys = stub
        .keys_error
        .clone()
        .unwrap_or_default()
        .replace('"', "'");
    println!(
        "{{\"event\":\"ready\",\"reader\":\"stub\",\"backend\":\"{}\",\"calls\":{},\"keys_error\":\"{}\"}}",
        cfg.flavor.name(),
        cfg.calls,
        keys
    );
    let mut sink = [0u8; 256];
    let mut stdin = std::io::stdin();
    while matches!(stdin.read(&mut sink), Ok(n) if n > 0) {}
    stub.stop();
    Ok(())
}
