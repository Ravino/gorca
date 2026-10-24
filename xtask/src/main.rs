// SPDX-License-Identifier: GPL-3.0-or-later
//! Repository automation. `cargo xtask deps` enforces the ADR-0001 section 11 dependency rules
//! over `cargo metadata`; see [`deps`].

mod cutover;
mod deps;
mod entryrule;
mod envrule;
mod ext;
mod hostfeature;
mod legacyrule;
mod strings;

use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("deps") => {
            let results = [
                deps::run(),
                envrule::run(),
                entryrule::run(),
                strings::run(false),
                legacyrule::run(false),
                hostfeature::run(),
            ];
            let mut ok = true;
            for r in results {
                match r {
                    Ok(true) => {}
                    Ok(false) => ok = false,
                    Err(e) => {
                        eprintln!("xtask deps: {e:#}");
                        return ExitCode::from(2);
                    }
                }
            }
            if ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Some("legacy") => {
            let print = args.next().as_deref() == Some("--print");
            match (legacyrule::run(print), hostfeature::run()) {
                (Ok(true), Ok(true)) => ExitCode::SUCCESS,
                (Ok(_), Ok(_)) => ExitCode::FAILURE,
                (Err(e), _) | (_, Err(e)) => {
                    eprintln!("xtask legacy: {e:#}");
                    ExitCode::from(2)
                }
            }
        }
        Some("cutover") => match cutover::run(args) {
            Ok(true) => ExitCode::SUCCESS,
            Ok(false) => ExitCode::FAILURE,
            Err(e) => {
                eprintln!("xtask cutover: {e:#}");
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
                "unknown xtask command `{other}`\n\nusage: cargo xtask deps | legacy [--print] | strings [--update] | build-ext [--dest DIR] | cutover <surface>|--check"
            );
            ExitCode::from(2)
        }
        None => {
            eprintln!(
                "usage: cargo xtask deps | legacy [--print] | strings [--update] | build-ext [--dest DIR] | cutover <surface>|--check"
            );
            ExitCode::from(2)
        }
    }
}
