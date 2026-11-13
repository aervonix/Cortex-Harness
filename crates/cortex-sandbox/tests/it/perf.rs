//! What a look at protected paths costs on a real workspace: a
//! measurement, run by hand in a release build, outside nextest.
//!
//! ```text
//! CORTEX_PERF_DIR=<a workspace> cargo test --release -p cortex-sandbox --test it perf_ -- --ignored --nocapture
//! ```
//!
//! Without `CORTEX_PERF_DIR` it looks at this repository.

use std::path::PathBuf;
use std::time::Instant;

use cortex_sandbox::protect::{Limits, Watch};

#[test]
#[ignore = "a measurement, run by hand in a release build: see the module documentation"]
fn perf_a_look_at_the_protected_paths_of_a_workspace() {
    let workspace = std::env::var_os("CORTEX_PERF_DIR").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."),
        PathBuf::from,
    );
    let fixed: Vec<PathBuf> = [
        ".git/hooks",
        ".git/config",
        ".git/info",
        ".cortex",
        ".github/workflows",
    ]
    .iter()
    .map(|name| workspace.join(name))
    .collect();
    for (name, limits) in [
        ("the standard bounds", Limits::default()),
        (
            "no bound",
            Limits {
                directories: usize::MAX,
                time: std::time::Duration::from_secs(600),
                ..Limits::default()
            },
        ),
    ] {
        let watch = Watch::new(&workspace, fixed.clone()).with_limits(limits);
        let _ = watch.take();
        let mut times: Vec<f64> = (0..10)
            .map(|_| {
                let start = Instant::now();
                let _ = watch.take();
                start.elapsed().as_secs_f64() * 1000.0
            })
            .collect();
        times.sort_by(f64::total_cmp);
        let snapshot = watch.take();
        println!(
            "{name:<20} median {:>8.1} ms   {} entries   complete: {}",
            times[times.len() / 2],
            snapshot.len(),
            snapshot.is_complete()
        );
    }
}
