//! The hidden subcommand `__sandbox`: the real binary as the sandbox
//! launcher (`docs/architecture/06-permissions-and-sandbox.md`, "How it is
//! applied"), and what it adds to starting a command.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Instant;

use cortex_sandbox::policy::{Network, Policy, Reach};
use cortex_sandbox::support::{Assessment, detect};
use cortex_testkit::Scratch;

use crate::CORTEX;

/// The Landlock version the sandbox enforces with here, or `None`. On CI's
/// Linux runners it must enforce.
fn enforcing() -> Option<u8> {
    match detect().assess() {
        Assessment::Enforcing { landlock, .. } => Some(landlock),
        other => {
            assert!(
                !cfg!(target_os = "linux") || std::env::var_os("CI").is_none(),
                "CI must run the sandbox's tests on a kernel where it enforces: {other:?}"
            );
            None
        }
    }
}

/// Starts `cortex __sandbox -- program arguments…` in `cwd` with `policy`
/// on its standard input, and returns its exit code and output.
fn launch(policy: &Policy, cwd: &Path, program: &str, arguments: &[&str]) -> (Option<i32>, String) {
    let mut child = Command::new(CORTEX)
        .args(["__sandbox", "--", program])
        .args(arguments)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start the launcher");
    {
        let mut input = child.stdin.take().expect("its input");
        let _ = input.write_all(&policy.encode());
    }
    let output = child.wait_with_output().expect("wait for it");
    (
        output.status.code(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )
}

#[test]
fn sys_the_hidden_subcommand_confines_the_command_it_becomes() {
    let Some(landlock) = enforcing() else { return };
    let scratch = Scratch::outside("cortex-cli-sandbox", "launcher");
    let ws = scratch.dir("ws");
    let tmp = scratch.dir("tmp");
    let outside = scratch.dir("outside");
    let policy = Policy {
        read: Reach::All,
        write: vec![ws.clone(), tmp.clone()],
        network: Network::Off,
        temp: Some(tmp.clone()),
        landlock,
    };
    let script = format!(
        "echo in > in.txt; echo t > \"$TMPDIR/t\"; echo out > '{}/f' || echo refused",
        outside.display()
    );
    let (code, output) = launch(&policy, &ws, "/bin/sh", &["-c", &script]);
    assert_eq!(code, Some(0), "{output}");
    assert!(output.contains("refused"), "{output}");
    assert!(ws.join("in.txt").exists() && tmp.join("t").exists());
    assert!(!outside.join("f").exists());

    // A policy the launcher does not understand runs nothing.
    let mut garbled = policy.encode();
    garbled.truncate(garbled.len() - 1);
    let mut child = Command::new(CORTEX)
        .args(["__sandbox", "--", "/bin/sh", "-c", "echo ran > ran.txt"])
        .current_dir(&ws)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start the launcher");
    {
        let mut input = child.stdin.take().expect("its input");
        let _ = input.write_all(&garbled);
    }
    let output = child.wait_with_output().expect("wait for it");
    assert_eq!(
        output.status.code(),
        Some(cortex_sandbox::launch::FAILED),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!ws.join("ran.txt").exists());
}

/// The budget "starting a shell command, harness overhead including the
/// sandbox launcher" of `docs/performance-budgets.md`: what starting a
/// command through the launcher adds to starting it directly. A
/// measurement, run by hand in a release build:
///
/// ```text
/// cargo test --release -p cortex-cli --test system perf_ -- --ignored --nocapture
/// ```
///
/// `CORTEX_PERF_RUNS` sets the number of runs of each case (200 unless
/// given).
#[test]
#[ignore = "a measurement, run by hand in a release build: see its documentation"]
fn perf_starting_a_command_through_the_launcher() {
    let Some(landlock) = enforcing() else {
        println!("the sandbox does not enforce here: nothing to measure");
        return;
    };
    let runs: usize = std::env::var("CORTEX_PERF_RUNS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(200);
    let scratch = Scratch::outside("cortex-cli-sandbox", "perf");
    let ws = scratch.dir("ws");
    let tmp = scratch.dir("tmp");
    let policy = Policy {
        read: Reach::All,
        write: vec![ws.clone(), tmp.clone()],
        network: Network::Off,
        temp: Some(tmp),
        landlock,
    };
    let bytes = policy.encode();
    let measure = |name: &str, one: &mut dyn FnMut()| {
        for _ in 0..10 {
            one();
        }
        let mut times: Vec<f64> = (0..runs)
            .map(|_| {
                let start = Instant::now();
                one();
                start.elapsed().as_secs_f64() * 1000.0
            })
            .collect();
        times.sort_by(f64::total_cmp);
        println!(
            "{name:<48} median {:>7.3} ms   p95 {:>7.3} ms",
            times[times.len() / 2],
            times[times.len() * 95 / 100]
        );
        times[times.len() / 2]
    };
    let bash = ["--noprofile", "--norc", "-c", "exit 0"];
    let direct = measure("bash alone", &mut || {
        let status = Command::new("bash")
            .args(bash)
            .current_dir(&ws)
            .stdin(Stdio::null())
            .status()
            .expect("run bash");
        assert!(status.success());
    });
    let launched = measure("bash through `cortex __sandbox`", &mut || {
        let mut child = Command::new(CORTEX)
            .args(["__sandbox", "--", "bash"])
            .args(bash)
            .current_dir(&ws)
            .stdin(Stdio::piped())
            .spawn()
            .expect("start the launcher");
        let mut input = child.stdin.take().expect("its input");
        input.write_all(&bytes).expect("the policy");
        drop(input);
        assert!(child.wait().expect("wait").success());
    });
    println!(
        "the launcher adds {:.3} ms (Landlock {landlock})",
        launched - direct
    );
}
