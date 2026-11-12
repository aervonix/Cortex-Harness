//! Turns `catalog/models.toml` into the JSON this crate embeds
//! (`docs/decisions/0017-model-catalog.md`).
//!
//! The catalog is written in TOML, because people edit it, and embedded as
//! JSON, because the crate already links a JSON reader and so needs no TOML
//! reader at run time. This script only changes the syntax. What the entries
//! mean, and whether they are valid, is the crate's business: its tests load
//! the embedded catalog and fail when an entry is wrong.

#![allow(
    clippy::disallowed_methods,
    reason = "a build script reads and writes files and has no async runtime to block"
)]

use std::path::PathBuf;

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(problem) => {
            use std::io::Write as _;
            // Cargo shows a failed build script's standard error.
            let _ = writeln!(std::io::stderr(), "cortex-llm build script: {problem}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let manifest_dir = var("CARGO_MANIFEST_DIR")?;
    let source = PathBuf::from(manifest_dir).join("../../catalog/models.toml");
    // Printed before anything can fail, so that fixing the file reruns this.
    println!("cargo::rerun-if-changed={}", source.display());
    println!("cargo::rerun-if-changed=build.rs");

    let text = std::fs::read_to_string(&source)
        .map_err(|error| format!("cannot read {}: {error}", source.display()))?;
    let catalog: serde_json::Value =
        toml::from_str(&text).map_err(|error| format!("catalog/models.toml: {error}"))?;
    let json = serde_json::to_string(&catalog)
        .map_err(|error| format!("cannot write the catalog as JSON: {error}"))?;

    let out = PathBuf::from(var("OUT_DIR")?).join("catalog.json");
    std::fs::write(&out, json).map_err(|error| format!("cannot write {}: {error}", out.display()))
}

fn var(name: &str) -> Result<String, String> {
    std::env::var(name).map_err(|_| format!("cargo did not set {name}"))
}
