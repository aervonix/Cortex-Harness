//! `cargo xtask startup BINARY [--runs N] [--budget desktop|linux|windows]`:
//! the start-up budgets of `docs/performance-budgets.md`, "Start-up",
//! measured on a built binary (step 13b).
//!
//! - **`--version`**, from starting the process to its exit, under both of
//!   its names: the binary itself, and a hard link (or a copy) named `cx`
//!   beside it in a directory of its own (`docs/decisions/0013-naming.md`).
//! - **A trivial headless run**, from starting the process to its request
//!   reaching the network: `cortex -p hi` against a listener of this
//!   command's own on the loopback interface, which reads the request's
//!   head and closes the connection. The run then ends with its own error;
//!   only the time to the request is counted. Its home, configuration,
//!   state and data are in a temporary directory, so nothing of the
//!   machine's own set-up is read.
//!
//! Each is run `--runs` times (30 by default); the median and the 95th
//! percentile are printed against the budget of the column `--budget`
//! names, and added to the job's summary in CI. With `--check` a median
//! over its budget fails the command.

use std::fs;
use std::io::Read;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::Result;
use crate::steps;

/// The budgets, in milliseconds: `--version`, and a headless run to its
/// request (`docs/performance-budgets.md`, "Start-up").
fn budget(column: &str) -> Result<(f64, f64)> {
    match column {
        "desktop" => Ok((5.0, 15.0)),
        "linux" => Ok((15.0, 40.0)),
        "windows" => Ok((50.0, 100.0)),
        other => Err(format!(
            "`{other}` is not a column of the start-up budgets: desktop, linux or windows"
        )),
    }
}

/// `cargo xtask startup …`
pub(crate) fn run(arguments: &[String]) -> Result<()> {
    let mut binary = None;
    let mut runs = 30_usize;
    let mut column = if cfg!(windows) { "windows" } else { "linux" }.to_owned();
    let mut check = false;
    let mut rest = arguments.iter();
    while let Some(argument) = rest.next() {
        match argument.as_str() {
            "--runs" => {
                runs = rest
                    .next()
                    .and_then(|value| value.parse().ok())
                    .filter(|runs| *runs > 0)
                    .ok_or("`--runs` takes a number above zero")?;
            }
            "--budget" => column = rest.next().ok_or("`--budget` takes a column")?.clone(),
            "--check" => check = true,
            flag if flag.starts_with('-') => return Err(format!("unknown flag `{flag}`")),
            path => binary = Some(PathBuf::from(path)),
        }
    }
    let binary = binary.ok_or(
        "usage: cargo xtask startup BINARY [--runs N] [--budget desktop|linux|windows] [--check]",
    )?;
    let binary =
        fs::canonicalize(&binary).map_err(|error| format!("{}: {error}", binary.display()))?;
    let (version_budget, headless_budget) = budget(&column)?;

    let scratch = std::env::temp_dir().join(format!("cortex-startup-{}", std::process::id()));
    let _ = fs::remove_dir_all(&scratch);
    fs::create_dir_all(&scratch).map_err(|error| format!("{}: {error}", scratch.display()))?;
    let outcome = measure(&binary, &scratch, runs);
    let _ = fs::remove_dir_all(&scratch);
    let rows = outcome?;

    let mut table = format!(
        "### Start-up, against the `{column}` budgets\n\n| What | Median | 95th percentile | Budget |\n|---|---:|---:|---:|\n"
    );
    let mut over = Vec::new();
    for (what, times) in &rows {
        let limit = if what.starts_with("headless") {
            headless_budget
        } else {
            version_budget
        };
        let (median, p95) = (percentile(times, 50), percentile(times, 95));
        table.push_str(&format!(
            "| {what} | {median:.2} ms | {p95:.2} ms | {limit:.0} ms |\n"
        ));
        if median > limit {
            over.push(format!("{what}: {median:.2} ms against {limit:.0} ms"));
        }
    }
    println!("\n{table}");
    steps::append_job_summary(&format!("{table}\n{runs} runs each.\n\n"));
    if check && !over.is_empty() {
        return Err(format!("over the start-up budget: {}", over.join("; ")));
    }
    Ok(())
}

fn measure(binary: &Path, scratch: &Path, runs: usize) -> Result<Vec<(String, Vec<f64>)>> {
    let alias_dir = scratch.join("bin");
    fs::create_dir_all(&alias_dir).map_err(|error| error.to_string())?;
    let alias = alias_dir.join(format!("cx{}", std::env::consts::EXE_SUFFIX));
    if fs::hard_link(binary, &alias).is_err() {
        fs::copy(binary, &alias).map_err(|error| format!("could not make `cx`: {error}"))?;
    }
    let home = scratch.join("home");
    let workspace = scratch.join("work");
    fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
    let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|error| error.to_string())?;
    let port = listener
        .local_addr()
        .map_err(|error| error.to_string())?
        .port();
    let environment = environment(&home, port)?;

    let mut rows = Vec::new();
    for (name, program) in [("cortex", binary), ("cx", alias.as_path())] {
        // The first run fills the file cache; it is not counted.
        let mut times = Vec::with_capacity(runs);
        for run in 0..=runs {
            let started = Instant::now();
            let status = Command::new(program)
                .arg("--version")
                .stdout(Stdio::null())
                .status()
                .map_err(|error| format!("could not run {}: {error}", program.display()))?;
            let elapsed = started.elapsed();
            if !status.success() {
                return Err(format!("`{name} --version` failed: {status}"));
            }
            if run > 0 {
                times.push(millis(elapsed));
            }
        }
        rows.push((format!("`{name} --version`"), times));
    }
    for (name, program) in [("cortex", binary), ("cx", alias.as_path())] {
        let mut times = Vec::with_capacity(runs);
        for run in 0..=runs {
            let started = Instant::now();
            let mut child = Command::new(program)
                .args(["-p", "hi", "-q"])
                .current_dir(&workspace)
                .envs(environment.iter().map(|(name, value)| (name, value)))
                .env_remove("CORTEX_LOG")
                .env_remove("CORTEX_CONFIG")
                .env_remove("CORTEX_DATA_DIR")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|error| format!("could not run {}: {error}", program.display()))?;
            let (mut connection, _) = listener.accept().map_err(|error| error.to_string())?;
            read_head(&mut connection)?;
            let elapsed = started.elapsed();
            drop(connection);
            let _ = child.wait();
            if run > 0 {
                times.push(millis(elapsed));
            }
        }
        rows.push((format!("headless `{name} -p`, to its request"), times));
    }
    Ok(rows)
}

/// The environment of a headless run: a home of its own, a configuration
/// that sends the request to `port`, and a key.
fn environment(home: &Path, port: u16) -> Result<Vec<(String, String)>> {
    let path = |parts: &[&str]| {
        let mut path = home.to_path_buf();
        path.extend(parts);
        path.to_string_lossy().into_owned()
    };
    let variables = vec![
        ("HOME".to_owned(), path(&[])),
        ("USERPROFILE".to_owned(), path(&[])),
        ("XDG_CONFIG_HOME".to_owned(), path(&[".config"])),
        ("XDG_STATE_HOME".to_owned(), path(&[".local", "state"])),
        ("XDG_DATA_HOME".to_owned(), path(&[".local", "share"])),
        ("XDG_CACHE_HOME".to_owned(), path(&[".cache"])),
        ("APPDATA".to_owned(), path(&["AppData", "Roaming"])),
        ("LOCALAPPDATA".to_owned(), path(&["AppData", "Local"])),
        ("ANTHROPIC_API_KEY".to_owned(), "sk-startup".to_owned()),
    ];
    let config = if cfg!(windows) {
        PathBuf::from(path(&["AppData", "Roaming", "cortex-harness"]))
    } else {
        PathBuf::from(path(&[".config", "cortex-harness"]))
    };
    fs::create_dir_all(&config).map_err(|error| error.to_string())?;
    fs::write(
        config.join("config.toml"),
        format!(
            "session.durability = \"never\"\ncredentials.store = \"file\"\nmodel.main = \
             \"claude-haiku-5-5\"\nretry.attempts = 1\nproviders.anthropic.base_url = \
             \"http://127.0.0.1:{port}\"\n"
        ),
    )
    .map_err(|error| error.to_string())?;
    Ok(variables)
}

/// Reads until the end of a request's head.
fn read_head(connection: &mut std::net::TcpStream) -> Result<()> {
    connection
        .set_read_timeout(Some(Duration::from_secs(30)))
        .map_err(|error| error.to_string())?;
    let mut head = Vec::new();
    let mut buffer = [0_u8; 4096];
    while !head.windows(4).any(|window| window == b"\r\n\r\n") {
        let read = connection
            .read(&mut buffer)
            .map_err(|error| format!("the request did not arrive: {error}"))?;
        if read == 0 {
            return Err("the connection closed before the request arrived".to_owned());
        }
        head.extend_from_slice(&buffer[..read]);
    }
    Ok(())
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

/// The `percent` percentile of `times`, by the nearest rank.
fn percentile(times: &[f64], percent: usize) -> f64 {
    let mut sorted = times.to_vec();
    sorted.sort_by(f64::total_cmp);
    let rank = (percent * sorted.len()).div_ceil(100).max(1);
    sorted[rank - 1]
}

#[cfg(test)]
mod tests {
    use super::percentile;

    #[test]
    fn percentiles_are_by_the_nearest_rank() {
        let times: Vec<f64> = (1..=20).map(f64::from).collect();
        assert!((percentile(&times, 50) - 10.0).abs() < f64::EPSILON);
        assert!((percentile(&times, 95) - 19.0).abs() < f64::EPSILON);
        assert!((percentile(&[3.0], 95) - 3.0).abs() < f64::EPSILON);
    }
}
