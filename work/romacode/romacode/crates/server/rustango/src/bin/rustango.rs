//! The `rustango` command-line binary — a thin wrapper over
//! `rustango::management::run`.

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = rustango::management::run(&args);
    ExitCode::from(code.clamp(0, 255) as u8)
}
