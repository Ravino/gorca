//! Repository automation. `cargo xtask deps` enforces the ADR-0001 section 11 dependency rules
//! over `cargo metadata`; see [`deps`].

mod deps;

use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("deps") => match deps::run() {
            Ok(true) => ExitCode::SUCCESS,
            Ok(false) => ExitCode::FAILURE,
            Err(e) => {
                eprintln!("xtask deps: {e:#}");
                ExitCode::from(2)
            }
        },
        Some(other) => {
            eprintln!("unknown xtask command `{other}`\n\nusage: cargo xtask deps");
            ExitCode::from(2)
        }
        None => {
            eprintln!("usage: cargo xtask deps");
            ExitCode::from(2)
        }
    }
}
