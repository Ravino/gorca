// SPDX-License-Identifier: GPL-3.0-or-later
//! Repository automation. `cargo xtask deps` enforces the ADR-0001 section 11 dependency rules
//! over `cargo metadata`; see [`deps`].

mod deps;
mod envrule;
mod ext;
mod strings;
mod unmasked;

use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("deps") => match (
            deps::run(),
            envrule::run(),
            strings::run(false),
            unmasked::run(),
        ) {
            (Ok(true), Ok(true), Ok(true), Ok(true)) => ExitCode::SUCCESS,
            (Ok(_), Ok(_), Ok(_), Ok(_)) => ExitCode::FAILURE,
            (Err(e), ..) | (_, Err(e), ..) | (_, _, Err(e), _) | (_, _, _, Err(e)) => {
                eprintln!("xtask deps: {e:#}");
                ExitCode::from(2)
            }
        },
        Some("strings") => match strings::run(args.next().as_deref() == Some("--update")) {
            Ok(true) => ExitCode::SUCCESS,
            Ok(false) => ExitCode::FAILURE,
            Err(e) => {
                eprintln!("xtask strings: {e:#}");
                ExitCode::from(2)
            }
        },
        Some("build-ext") => match ext::run(args) {
            Ok(true) => ExitCode::SUCCESS,
            Ok(false) => ExitCode::FAILURE,
            Err(e) => {
                eprintln!("xtask build-ext: {e:#}");
                ExitCode::from(2)
            }
        },
        Some(other) => {
            eprintln!(
                "unknown xtask command `{other}`\n\nusage: cargo xtask deps | strings [--update] | build-ext [--dest DIR]"
            );
            ExitCode::from(2)
        }
        None => {
            eprintln!("usage: cargo xtask deps | strings [--update] | build-ext [--dest DIR]");
            ExitCode::from(2)
        }
    }
}
