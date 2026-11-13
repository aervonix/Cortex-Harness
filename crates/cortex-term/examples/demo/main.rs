//! The demo binary: fake streamed text on the terminal layer. Its look is a
//! placeholder; see `app.rs`.
//!
//! ```text
//! cargo run --release -p cortex-term --example demo --features demo -- rate=2000 auto
//! ```

#![allow(
    clippy::print_stderr,
    reason = "a program of its own: it reports a wrong option on standard error"
)]

use std::process::ExitCode;

#[allow(
    unreachable_pub,
    reason = "the module is shared with the test binary, which needs it public"
)]
mod app;

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let options = match app::Options::parse(arguments.iter().map(String::as_str)) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("demo: {message}");
            return ExitCode::FAILURE;
        }
    };
    match app::run(&options) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("demo: {error}");
            ExitCode::FAILURE
        }
    }
}
