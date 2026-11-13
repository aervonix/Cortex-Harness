//! The generated configuration files in `docs/configuration/` are what the
//! settings generate (`docs/architecture/08-configuration.md`,
//! "Validation").
//!
//! `cargo xtask gen` writes them by running this test with
//! `CORTEX_UPDATE_GENERATED` set. Without it the test compares, on both
//! OSes, so a key cannot be added, reclassified or redocumented without the
//! schema and the reference page changing in the same pull request.

use std::fs;
use std::path::{Path, PathBuf};

use cortex_config::schema::{json_schema, reference_page};

fn docs() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("docs")
        .join("configuration")
}

#[test]
fn the_generated_configuration_files_are_current() {
    let files = [
        ("schema.json", json_schema()),
        ("reference.md", reference_page()),
    ];
    if std::env::var_os("CORTEX_UPDATE_GENERATED").is_some() {
        fs::create_dir_all(docs()).unwrap();
        for (name, content) in &files {
            let path = docs().join(name);
            if fs::read_to_string(&path).is_ok_and(|current| current == *content) {
                println!("unchanged  docs/configuration/{name}");
            } else {
                fs::write(&path, content).unwrap();
                println!("wrote      docs/configuration/{name}");
            }
        }
        return;
    }
    let stale: Vec<&str> = files
        .iter()
        .filter(|(name, content)| {
            let current = fs::read_to_string(docs().join(name)).unwrap_or_default();
            // A checkout on Windows may have turned the line endings.
            current.replace("\r\n", "\n") != *content
        })
        .map(|(name, _)| *name)
        .collect();
    assert!(
        stale.is_empty(),
        "stale in docs/configuration: {}. The settings in crates/cortex-config changed and the \
         generated files did not. Run `cargo xtask gen` and commit the result: the difference is \
         the change to the configuration, and belongs in the review.",
        stale.join(", ")
    );
}
