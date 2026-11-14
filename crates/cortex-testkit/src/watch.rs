//! What a test was doing when it ran out of time (`docs/testing-and-ci.md`,
//! "When a test runs out of time").
//!
//! nextest stops a test at its tier's limit and says only that it timed
//! out. A test that hangs on a gate, a socket or a child has nothing to say
//! for itself by then: it is killed. So the first time a test process uses
//! one of the test kit's waits (a [`Gate`](crate::Gate), the scripted
//! server, a [`Roll`](crate::children::Roll), [`until`](crate::children::until))
//! this module starts one thread that does nothing until the test has used
//! four fifths of its limit. If the test is still running then, the thread
//! writes a report to the test's standard error, which nextest keeps and
//! shows with the failure, in the log and in the JUnit report:
//!
//! - **what each waiting thread is waiting for:** which gate (by where it
//!   was made), which connection of the scripted server and which step of
//!   its script, which child of a roll;
//! - **the process tree under the test,** each process with its state and
//!   its command line; on Linux each thread's name, state and the kernel
//!   function it sleeps in (its wait channel), and its kernel stack where
//!   the system lets a process read it (as root);
//! - on Windows, the tree, each process's command line and each thread's
//!   state and wait reason, asked of PowerShell. Thread stacks are not read
//!   there.
//!
//! A test that passes never reaches the report. What it costs is one
//! thread that waits, and a lock taken when a wait starts and ends.
//!
//! The thread runs only under nextest (`NEXTEST` is set) and never in a
//! child that a test started from its own binary. It reads the limit from
//! the profile nextest names and the test's name; a test of the watch
//! itself shortens it with `CORTEX_WATCH_LIMIT_MS`.
//!
//! # Widening a race on purpose
//!
//! `CORTEX_TESTKIT_PAUSE_MS=<n>` makes the scripted server pause `n`
//! milliseconds before each write and before opening each gate of its
//! script. A race between what the server writes and what the test does
//! next, which a loaded machine loses once in a thousand runs, then loses
//! every time, so that a fix can be shown to hold. It is off unless set,
//! and nothing else reads it.

#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "test support: it reads /proc and asks PowerShell, on a thread of its own, only when a test is about to be stopped"
)]

use std::fmt::Write as _;
use std::io::Write as _;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

/// How much of its limit a test uses before the report is written.
const SHARE: f64 = 0.8;

/// What one thread is doing, for as long as the handle lives.
#[derive(Debug)]
pub struct Doing {
    id: u64,
    /// The same wait in the time profile, when it is on: none when it is
    /// off, so that a wait takes no further lock.
    mark: Option<Mutex<crate::profile::Mark>>,
}

struct Entry {
    id: u64,
    thread: String,
    since: Instant,
    what: String,
}

fn entries() -> &'static Mutex<Vec<Entry>> {
    static ENTRIES: Mutex<Vec<Entry>> = Mutex::new(Vec::new());
    &ENTRIES
}

/// Says what the calling thread is doing until the handle is dropped, and
/// starts the watch if it has not started. For a test's own long wait that
/// none of the test kit's waits covers.
#[must_use]
pub fn doing(what: impl Into<String>) -> Doing {
    arm();
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let thread = std::thread::current();
    let what = what.into();
    let mark = crate::profile::enabled().then(|| Mutex::new(crate::profile::wait(|| what.clone())));
    let entry = Entry {
        id,
        thread: thread.name().unwrap_or("an unnamed thread").to_owned(),
        since: Instant::now(),
        what,
    };
    lock().push(entry);
    Doing { id, mark }
}

impl Doing {
    /// Says what the thread is doing now.
    pub fn now(&self, what: impl Into<String>) {
        let what = what.into();
        if let Some(mark) = &self.mark {
            mark.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .next(|| what.clone());
        }
        if let Some(entry) = lock().iter_mut().find(|entry| entry.id == self.id) {
            entry.what = what;
            entry.since = Instant::now();
        }
    }
}

impl Drop for Doing {
    fn drop(&mut self) {
        lock().retain(|entry| entry.id != self.id);
    }
}

fn lock() -> std::sync::MutexGuard<'static, Vec<Entry>> {
    entries().lock().unwrap_or_else(PoisonError::into_inner)
}

/// Starts the watch for this test process, once. Does nothing outside
/// nextest, in a child a test started from its own binary, or for a test
/// whose limit is not known.
pub fn arm() {
    static ARMED: OnceLock<()> = OnceLock::new();
    ARMED.get_or_init(|| {
        let Some((limit, why)) = limit() else { return };
        let age = process_age().unwrap_or_default();
        let wait = report_at(limit).saturating_sub(age);
        let started = Instant::now();
        // If the thread cannot be started, the test runs without a watch.
        let _ = std::thread::Builder::new()
            .name("cortex-testkit-watch".to_owned())
            .spawn(move || {
                pause(wait);
                // The waits first, which cost nothing to gather, and only
                // then the tree, which on Windows asks PowerShell and can
                // take longer than the test has left.
                let (head, tail) = report(started.elapsed() + age, limit, &why);
                let mut error = std::io::stderr().lock();
                let _ = error.write_all(head.as_bytes());
                let _ = error.flush();
                let _ = error.write_all(tail().as_bytes());
                let _ = error.flush();
            });
    });
}

/// How far into a test's limit the report is written.
fn report_at(limit: Duration) -> Duration {
    limit.mul_f64(SHARE)
}

/// The test's limit and where it came from, or none when the watch should
/// not run.
fn limit() -> Option<(Duration, String)> {
    std::env::var_os("NEXTEST")?;
    // A child that a test started from its own binary: its parent watches.
    if std::env::args().any(|argument| argument.starts_with("cx-child=") || argument == "cx-echo") {
        return None;
    }
    if let Some(milliseconds) = std::env::var("CORTEX_WATCH_LIMIT_MS")
        .ok()
        .and_then(|text| text.parse().ok())
    {
        return Some((
            Duration::from_millis(milliseconds),
            "CORTEX_WATCH_LIMIT_MS".to_owned(),
        ));
    }
    let profile = std::env::var("NEXTEST_PROFILE").unwrap_or_default();
    let test = std::env::var("NEXTEST_TEST_NAME")
        .ok()
        .or_else(|| std::thread::current().name().map(str::to_owned))
        .unwrap_or_default();
    let seconds = limit_of(&profile, &test);
    Some((
        Duration::from_secs(seconds),
        format!("nextest's `{profile}` profile, for `{test}`"),
    ))
}

/// The limit in seconds nextest gives a test in a profile, as
/// `.config/nextest.toml` sets it. A test checks the two agree.
fn limit_of(profile: &str, test: &str) -> u64 {
    let system = test.starts_with("sys_") || test.contains("::sys_");
    match (profile, system) {
        ("properties", false) => 300,
        (_, true) => 60,
        (_, false) => 10,
    }
}

/// How long ago this process started, where the system says.
pub(crate) fn process_age() -> Option<Duration> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    // The fields after the command's name, which is in brackets and may
    // hold spaces; the start time is the 22nd field, the 20th after it.
    let after = &stat[stat.rfind(')')? + 2..];
    let ticks: f64 = after.split_whitespace().nth(19)?.parse().ok()?;
    let uptime: f64 = std::fs::read_to_string("/proc/uptime")
        .ok()?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    // Linux counts a process's start in ticks of 1/100 s on every
    // platform this project builds for.
    Some(Duration::from_secs_f64((uptime - ticks / 100.0).max(0.0)))
}

/// Waits for `length` without sleeping a fixed time: parked, and woken
/// early only by chance, so it parks again for what is left.
fn pause(length: Duration) {
    let end = Instant::now() + length;
    loop {
        let now = Instant::now();
        if now >= end {
            return;
        }
        std::thread::park_timeout(end - now);
    }
}

/// The pause `CORTEX_TESTKIT_PAUSE_MS` asks the scripted server for, if
/// any.
pub(crate) fn race_pause() {
    static PAUSE: OnceLock<Option<Duration>> = OnceLock::new();
    let pause_for = PAUSE.get_or_init(|| {
        std::env::var("CORTEX_TESTKIT_PAUSE_MS")
            .ok()
            .and_then(|text| text.parse().ok())
            .map(Duration::from_millis)
    });
    if let Some(length) = pause_for {
        pause(*length);
    }
}

/// The report: what the test's threads wait for, and then, gathered only
/// when it is asked for, its process tree.
fn report(ran: Duration, limit: Duration, why: &str) -> (String, impl FnOnce() -> String) {
    let mut text = format!(
        "\n== cortex-testkit: this test has run {:.1} s of the {:.0} s it is given ({why}). \
         What it is doing:\n",
        ran.as_secs_f64(),
        limit.as_secs_f64()
    );
    text.push_str(&waits());
    let tree = || {
        let mut tail = tree(std::process::id());
        tail.push_str("== end of cortex-testkit's report\n");
        tail
    };
    (text, tree)
}

/// The whole report at once, for a test.
#[cfg(test)]
fn report_text(ran: Duration, limit: Duration, why: &str) -> String {
    let (head, tail) = report(ran, limit, why);
    head + &tail()
}

/// What each thread said it is doing, longest first.
fn waits() -> String {
    let entries = lock();
    let mut lines: Vec<(Duration, String)> = entries
        .iter()
        .map(|entry| {
            let since = entry.since.elapsed();
            (
                since,
                format!(
                    "  {:>6.1} s  thread `{}`: {}\n",
                    since.as_secs_f64(),
                    entry.thread,
                    entry.what
                ),
            )
        })
        .collect();
    drop(entries);
    lines.sort_by_key(|line| std::cmp::Reverse(line.0));
    let mut text = String::from("waits, and for how long:\n");
    if lines.is_empty() {
        text.push_str("  none of the test kit's waits is in progress\n");
    }
    for (_, line) in lines {
        text.push_str(&line);
    }
    text
}

/// The process tree under `root`.
fn tree(root: u32) -> String {
    if cfg!(target_os = "linux") {
        linux::tree(root)
    } else if cfg!(windows) {
        windows_tree(root)
    } else {
        "processes: not read on this OS\n".to_owned()
    }
}

mod linux {
    //! The process tree from `/proc`.

    use std::fmt::Write as _;

    /// A process's parent, state and name, from `/proc/<pid>/stat`.
    fn stat(path: &str) -> Option<(u32, char, String)> {
        let stat = std::fs::read_to_string(path).ok()?;
        let open = stat.find('(')?;
        let close = stat.rfind(')')?;
        let name = stat[open + 1..close].to_owned();
        let mut rest = stat[close + 2..].split_whitespace();
        let state = rest.next()?.chars().next()?;
        let parent = rest.next()?.parse().ok()?;
        Some((parent, state, name))
    }

    pub(super) fn tree(root: u32) -> String {
        let mut parents = Vec::new();
        if let Ok(directory) = std::fs::read_dir("/proc") {
            for entry in directory.flatten() {
                let Some(pid) = entry
                    .file_name()
                    .to_str()
                    .and_then(|name| name.parse::<u32>().ok())
                else {
                    continue;
                };
                if let Some((parent, _, _)) = stat(&format!("/proc/{pid}/stat")) {
                    parents.push((pid, parent));
                }
            }
        }
        let mut text = String::from(
            "processes: pid, state, command line; then each thread's name, state and wait channel\n",
        );
        let mut stack = vec![(root, 0usize)];
        while let Some((pid, depth)) = stack.pop() {
            let indent = "  ".repeat(depth + 1);
            let (_, state, name) =
                stat(&format!("/proc/{pid}/stat")).unwrap_or((0, '?', String::new()));
            let command = std::fs::read(format!("/proc/{pid}/cmdline"))
                .map(|bytes| {
                    let line = String::from_utf8_lossy(&bytes).replace('\0', " ");
                    let line = line.trim_end().to_owned();
                    if line.is_empty() {
                        format!("[{name}]")
                    } else {
                        line
                    }
                })
                .unwrap_or_else(|_| format!("[{name}]"));
            let command: String = command.chars().take(400).collect();
            let _ = writeln!(text, "{indent}{pid} {state} {command}");
            if let Ok(tasks) = std::fs::read_dir(format!("/proc/{pid}/task")) {
                let mut tids: Vec<u32> = tasks
                    .flatten()
                    .filter_map(|task| task.file_name().to_str().and_then(|name| name.parse().ok()))
                    .collect();
                tids.sort_unstable();
                for tid in tids {
                    let base = format!("/proc/{pid}/task/{tid}");
                    let (_, state, name) =
                        stat(&format!("{base}/stat")).unwrap_or((0, '?', String::new()));
                    let channel =
                        std::fs::read_to_string(format!("{base}/wchan")).unwrap_or_default();
                    let channel = if channel.is_empty() || channel == "0" {
                        "-"
                    } else {
                        channel.trim()
                    };
                    let _ = writeln!(text, "{indent}  thread {tid} `{name}` {state} in {channel}");
                    // The kernel stack is readable as root only; elsewhere
                    // this is empty and costs one failed open.
                    if let Ok(stack) = std::fs::read_to_string(format!("{base}/stack")) {
                        for frame in stack.lines().take(8) {
                            let _ = writeln!(text, "{indent}      {frame}");
                        }
                    }
                }
            }
            let mut children: Vec<u32> = parents
                .iter()
                .filter(|(_, parent)| *parent == pid)
                .map(|(child, _)| *child)
                .collect();
            children.sort_unstable_by(|a, b| b.cmp(a));
            stack.extend(children.into_iter().map(|child| (child, depth + 1)));
        }
        text
    }
}

/// The process tree under `root` on Windows, from PowerShell: each
/// process's id, name and command line, and each of its threads' state and
/// wait reason. Thread stacks are not read.
fn windows_tree(root: u32) -> String {
    let script = format!(
        "$all = Get-CimInstance Win32_Process | Select-Object ProcessId,ParentProcessId,CommandLine; \
         $todo = [System.Collections.Generic.Queue[object]]::new(); $todo.Enqueue(@({root}, 1)); \
         while ($todo.Count -gt 0) {{ $item = $todo.Dequeue(); $id = $item[0]; $depth = $item[1]; \
           $p = $all | Where-Object {{ $_.ProcessId -eq $id }}; \
           $line = if ($p) {{ $p.CommandLine }} else {{ '?' }}; \
           Write-Output ('  ' * $depth + \"$id $line\"); \
           Get-CimInstance Win32_Thread -Filter \"ProcessHandle='$id'\" | ForEach-Object {{ \
             Write-Output ('  ' * $depth + \"  thread $($_.Handle) state $($_.ThreadState) wait reason $($_.ThreadWaitReason)\") }}; \
           $all | Where-Object {{ $_.ParentProcessId -eq $id -and $_.ProcessId -ne $id }} | ForEach-Object {{ $todo.Enqueue(@($_.ProcessId, ($depth + 1))) }} }}"
    );
    let output = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output();
    let mut text = String::from(
        "processes: pid and command line; then each thread's state and wait reason \
         (Win32_Thread: state 5 is waiting). Thread stacks are not read on Windows.\n",
    );
    match output {
        Ok(output) => text.push_str(&String::from_utf8_lossy(&output.stdout)),
        Err(error) => {
            let _ = writeln!(text, "  PowerShell could not be asked: {error}");
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpStream;
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    use std::time::Duration;

    use super::{doing, limit_of, process_age, report_at, report_text as report};

    #[test]
    fn the_report_comes_before_the_limit() {
        assert_eq!(report_at(Duration::from_secs(10)), Duration::from_secs(8));
        assert_eq!(report_at(Duration::from_secs(60)), Duration::from_secs(48));
    }

    #[test]
    fn the_age_of_this_process_is_read_on_linux() {
        let age = process_age();
        if cfg!(target_os = "linux") {
            // This test's process started a moment ago, not when the
            // machine did.
            let age = age.expect("Linux says when a process started");
            assert!(age < Duration::from_secs(120), "{age:?}");
        } else {
            assert_eq!(age, None);
        }
    }
    use crate::{Gate, ServerReply, SseServer};

    #[test]
    fn the_limits_are_those_nextest_is_given() {
        assert_eq!(limit_of("ci", "rules::tests::a_rule"), 10);
        assert_eq!(limit_of("default", "shell::sys_quoting"), 60);
        assert_eq!(limit_of("ci", "sys_version"), 60);
        assert_eq!(limit_of("properties", "cases::a_property"), 300);
        // The same numbers as the nextest configuration's.
        let configuration = include_str!("../../../.config/nextest.toml");
        for line in [
            r#"slow-timeout = { period = "2s", terminate-after = 5 }"#,
            r#"slow-timeout = { period = "10s", terminate-after = 6 }"#,
            r#"slow-timeout = { period = "60s", terminate-after = 5 }"#,
        ] {
            assert!(
                configuration.lines().any(|each| each.trim() == line),
                "the nextest configuration no longer says {line}: change limit_of with it"
            );
        }
    }

    #[test]
    fn a_wait_is_reported_while_it_lasts() {
        // The waits alone: the process tree is asked of PowerShell on
        // Windows, which costs a second a time, and is tested by the
        // system test below.
        let waits =
            |text: &str| super::report(Duration::from_secs(8), Duration::from_secs(10), text).0;
        let waiting = doing("waiting for something made up for this test");
        let text = waits("a test");
        assert!(text.contains("has run 8.0 s of the 10 s"), "{text}");
        assert!(
            text.contains("waiting for something made up for this test"),
            "{text}"
        );
        waiting.now("now doing something else");
        let text = waits("a test");
        assert!(text.contains("now doing something else"), "{text}");
        assert!(!text.contains("made up for this test"), "{text}");
        drop(waiting);
        let text = waits("a test");
        assert!(!text.contains("something else"), "{text}");
        // This process is in its own tree, read from /proc on Linux.
        if cfg!(target_os = "linux") {
            let whole = report(Duration::from_secs(8), Duration::from_secs(10), "a test");
            assert!(
                whole.contains(&format!(" {} ", std::process::id())),
                "{whole}"
            );
        }
    }

    /// The test that hangs on purpose: a gate nobody opens, a scripted
    /// server holding a response, and a child process that stays. It does
    /// nothing unless the test below starts it with `CORTEX_WATCH_HANG`.
    #[test]
    #[ignore = "run by sys_a_test_that_runs_out_of_time_says_what_it_was_doing"]
    fn hangs_on_purpose() {
        if std::env::var_os("CORTEX_WATCH_HANG").is_none() {
            return;
        }
        let server = SseServer::start();
        let never = Gate::new();
        server.push(ServerReply::sse().hold_until(&never).send("data: x\n\n"));
        let mut connection =
            TcpStream::connect(server.url("").trim_start_matches("http://")).unwrap();
        connection
            .write_all(b"POST /v1/held HTTP/1.1\r\nContent-Length: 0\r\n\r\n")
            .unwrap();
        // A child that stays until it is killed: this binary, waiting on
        // its standard input, which is never closed.
        let program = std::env::current_exe().unwrap();
        let mut child = Command::new(program)
            .args(["--exact", "watch::tests::stays", "--ignored"])
            .env("CORTEX_WATCH_STAY", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stuck = Gate::new();
        let _ = stuck.wait(Duration::from_secs(600));
        // Not reached: the test that started this one kills it first.
        let _ = child.kill();
        let _ = child.wait();
    }

    /// A scripted exchange of four writes (the head, two pieces, the end),
    /// which prints how long it took. It does nothing unless the test below
    /// starts it with `CORTEX_TESTKIT_PAUSE_MS`.
    #[test]
    #[ignore = "run by sys_the_race_pause_holds_back_each_write"]
    fn paused_exchange() {
        if std::env::var_os("CORTEX_TESTKIT_PAUSE_MS").is_none() {
            return;
        }
        let server = SseServer::start();
        server.push(ServerReply::sse().send("data: a\n\n").send("data: b\n\n"));
        let started = std::time::Instant::now();
        let mut connection =
            TcpStream::connect(server.url("").trim_start_matches("http://")).unwrap();
        connection
            .write_all(b"POST / HTTP/1.1\r\nContent-Length: 0\r\n\r\n")
            .unwrap();
        let mut response = String::new();
        std::io::Read::read_to_string(&mut connection, &mut response).unwrap();
        assert!(response.ends_with("0\r\n\r\n"), "{response}");
        println!("elapsed-ms: {}", started.elapsed().as_millis());
    }

    #[test]
    #[ignore = "waits on purpose; the nightly workflow runs it"]
    fn sys_the_race_pause_holds_back_each_write() {
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "watch::tests::paused_exchange",
                "--ignored",
                "--nocapture",
            ])
            .env("CORTEX_TESTKIT_PAUSE_MS", "100")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "{stdout}");
        let elapsed: u64 = stdout
            .lines()
            .find_map(|line| line.strip_prefix("elapsed-ms: "))
            .and_then(|number| number.trim().parse().ok())
            .unwrap_or_else(|| panic!("{stdout}"));
        // Four writes, each held back 100 ms.
        assert!(elapsed >= 400, "the exchange took {elapsed} ms: {stdout}");
    }

    /// The child of `hangs_on_purpose`.
    #[test]
    #[ignore = "a child of hangs_on_purpose"]
    fn stays() {
        if std::env::var_os("CORTEX_WATCH_STAY").is_none() {
            return;
        }
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
    }

    /// Kills a process and everything under it.
    fn kill_tree(child: &mut std::process::Child) {
        let pid = child.id().to_string();
        if cfg!(windows) {
            let _ = Command::new("taskkill")
                .args(["/T", "/F", "/PID", &pid])
                .output();
        } else {
            // The child leads a process group of its own.
            let _ = Command::new("kill")
                .args(["-KILL", &format!("-{pid}")])
                .output();
        }
        let _ = child.kill();
        let _ = child.wait();
    }

    /// Ignored in the blocking checks, where it would cost a second of the
    /// system tier on each OS (more on Windows, where the report asks
    /// PowerShell). The nightly workflow runs it on both OSes, and so does
    /// anyone who changes this module:
    /// `cargo xtask test -p cortex-testkit --run-ignored only watch::tests::sys_`.
    #[test]
    #[ignore = "starts a test that hangs on purpose; the nightly workflow runs it"]
    fn sys_a_test_that_runs_out_of_time_says_what_it_was_doing() {
        let program = std::env::current_exe().unwrap();
        let mut command = Command::new(program);
        command
            .args([
                "--exact",
                "watch::tests::hangs_on_purpose",
                "--ignored",
                "--nocapture",
            ])
            .env("NEXTEST", "1")
            .env("NEXTEST_PROFILE", "ci")
            .env("NEXTEST_TEST_NAME", "watch::tests::hangs_on_purpose")
            .env("CORTEX_WATCH_LIMIT_MS", "1000")
            .env("CORTEX_WATCH_HANG", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = command.spawn().unwrap();
        let stderr = child.stderr.take().unwrap();
        let (lines, received) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines() {
                let Ok(line) = line else { break };
                let end = line.starts_with("== end of cortex-testkit's report");
                if lines.send(line).is_err() || end {
                    break;
                }
            }
        });
        let mut text = String::new();
        // A bound on a mistake: the report comes after 0.8 s.
        while let Ok(line) = received.recv_timeout(Duration::from_secs(30)) {
            text.push_str(&line);
            text.push('\n');
            if line.starts_with("== end of cortex-testkit's report") {
                break;
            }
        }
        kill_tree(&mut child);

        assert!(
            text.contains("of the 1 s it is given (CORTEX_WATCH_LIMIT_MS)"),
            "{text}"
        );
        // Before the limit, at four fifths of it: counted from the
        // process's start on Linux, from the first wait elsewhere.
        let ran: f64 = text
            .split("this test has run ")
            .nth(1)
            .and_then(|rest| rest.split(' ').next())
            .and_then(|number| number.parse().ok())
            .unwrap_or_else(|| panic!("{text}"));
        assert!(
            (0.75..1.0).contains(&ran),
            "the report came at {ran} s: {text}"
        );
        // The test's own wait, by where the gate was made.
        assert!(
            text.contains("waits for the gate made at crates/cortex-testkit/src/watch.rs:")
                || text
                    .contains("waits for the gate made at crates\\cortex-testkit\\src\\watch.rs:"),
            "{text}"
        );
        // The server's connection, and what it holds for.
        assert!(
            text.contains(
                "connection 1, POST /v1/held: holding the response until the gate made at"
            ),
            "{text}"
        );
        // The tree: the test and its child.
        assert!(text.contains("hangs_on_purpose"), "{text}");
        assert!(text.contains("watch::tests::stays"), "{text}");
        if cfg!(target_os = "linux") {
            assert!(text.contains(" in "), "each thread's wait channel: {text}");
        }
        if cfg!(windows) {
            assert!(text.contains("wait reason"), "{text}");
        }
    }
}
