//! `cortex_sandbox::tree`: starting, interrupting, killing and ending a
//! tree, its output pipe, and what happens to it when the harness dies.

use std::io;

use cortex_sandbox::tree::{Exit, Output, ReadOutcome, Tree};
use cortex_testkit::children::{ChildSpec, Roll};

use crate::{BARE_HARNESS, ENTRY, HARNESS, LIMIT, harness};

/// Reads an output pipe to its end.
fn read_all(output: &mut Output) -> String {
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 4096];
    loop {
        match output.read(&mut buffer).expect("read the output") {
            ReadOutcome::Bytes(count) => bytes.extend_from_slice(&buffer[..count]),
            ReadOutcome::Closed => break,
            ReadOutcome::Stopped => panic!("nothing stopped the read"),
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

#[test]
fn sys_a_command_runs_and_says_how_it_ended() {
    let roll = Roll::new();
    let mut command = ChildSpec::new(&roll, "only")
        .saying("hello-from-the-child")
        .exiting(3)
        .command(ENTRY);
    let (mut output, _stop) = Output::attach(&mut command).expect("make the pipe");
    let tree = Tree::spawn(command, None).expect("start the command");

    let child = roll.next(LIMIT);
    assert_eq!(child.name(), "only");
    assert_eq!(child.pid(), tree.id());
    assert_eq!(tree.wait().expect("wait"), Exit::Code(3));
    // Asking again gives the same answer.
    assert_eq!(tree.wait().expect("wait again"), Exit::Code(3));
    assert!(!Exit::Code(3).success());
    assert!(Exit::Code(0).success());

    // The pipe closes by itself: the harness kept no end of it.
    let text = read_all(&mut output);
    assert!(text.contains("hello-from-the-child"), "{text:?}");
    assert!(child.gone_within(LIMIT));

    tree.end();
    for refused in [tree.interrupt(), tree.terminate(), tree.kill()] {
        let error = refused.expect_err("an ended tree takes no signal");
        // Windows has no terminate stage at all, and says so first.
        assert!(
            matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::Unsupported
            ),
            "{error}"
        );
    }
    // The exit is still known.
    assert_eq!(tree.wait().expect("wait after the end"), Exit::Code(3));
}

#[test]
fn sys_killing_a_tree_takes_the_grandchild_that_ignores_the_interrupt() {
    let roll = Roll::new();
    let command = ChildSpec::new(&roll, "parent")
        .with_child(ChildSpec::new(&roll, "grandchild").stubborn())
        .command(ENTRY);
    let tree = Tree::spawn(command, None).expect("start the command");
    let both = roll.all(&["parent", "grandchild"], LIMIT);
    assert!(both[0].is_alive() && both[1].is_alive());

    tree.kill().expect("kill the tree");
    assert!(both[0].gone_within(LIMIT), "the command survived the kill");
    assert!(
        both[1].gone_within(LIMIT),
        "the grandchild survived the kill"
    );

    let exit = tree.wait().expect("wait");
    if cfg!(unix) {
        assert_eq!(exit, Exit::Signal(9));
    } else {
        assert_eq!(exit, Exit::Code(1));
    }
    tree.end();
}

#[test]
fn sys_the_interrupt_stops_what_listens_and_leaves_what_does_not() {
    let roll = Roll::new();
    let command = ChildSpec::new(&roll, "parent")
        .with_child(ChildSpec::new(&roll, "grandchild").stubborn())
        .command(ENTRY);
    let tree = Tree::spawn(command, None).expect("start the command");
    let both = roll.all(&["parent", "grandchild"], LIMIT);

    tree.interrupt().expect("interrupt the tree");
    assert!(
        both[0].gone_within(LIMIT),
        "the command ignored the interrupt"
    );
    // The command's death shows that the interrupt was delivered, and the
    // grandchild is still there.
    assert!(both[1].is_alive(), "the grandchild did not ignore it");
    assert!(!tree.wait().expect("wait").success());

    if cfg!(unix) {
        // It ignores the next stage too.
        tree.terminate().expect("terminate the tree");
        assert!(both[1].is_alive());
    } else {
        // The command has exited, so the event has nobody to be addressed
        // to; and Windows has no stage between the event and the kill.
        assert!(tree.interrupt().is_err());
        let error = tree.terminate().expect_err("no terminate stage");
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    }

    tree.end();
    assert!(
        both[1].gone_within(LIMIT),
        "the grandchild outlived the tree"
    );
}

#[test]
fn sys_ending_a_tree_kills_what_the_command_left_running() {
    let roll = Roll::new();
    let command = ChildSpec::new(&roll, "parent")
        .exiting(0)
        .with_child(ChildSpec::new(&roll, "left-behind"))
        .command(ENTRY);
    let tree = Tree::spawn(command, None).expect("start the command");
    let both = roll.all(&["parent", "left-behind"], LIMIT);

    assert_eq!(tree.wait().expect("wait"), Exit::Code(0));
    assert!(both[0].gone_within(LIMIT));
    assert!(
        both[1].is_alive(),
        "what the command started outlives the command"
    );
    assert_eq!(
        tree.others_running().expect("count"),
        1,
        "the one it left is counted, and the command that exited is not"
    );

    tree.end();
    assert!(both[1].gone_within(LIMIT), "it outlived the tree");
}

/// The same for a process the command left that has no console of its own
/// (Windows) or ignores the polite signals (Linux): it is in the tree, it
/// is counted, and the tree's end takes it.
#[test]
fn sys_a_detached_process_the_command_left_is_counted_and_killed() {
    let roll = Roll::new();
    let command = ChildSpec::new(&roll, "parent")
        .exiting(0)
        .with_child(ChildSpec::new(&roll, "detached").stubborn())
        .command(ENTRY);
    let tree = Tree::spawn(command, None).expect("start the command");
    let both = roll.all(&["parent", "detached"], LIMIT);

    assert_eq!(tree.wait().expect("wait"), Exit::Code(0));
    assert!(both[0].gone_within(LIMIT));
    assert_eq!(tree.others_running().expect("count"), 1);
    assert!(both[1].is_alive());

    tree.end();
    assert!(both[1].gone_within(LIMIT), "it outlived the tree");
}

/// A command that exits and leaves nothing is never counted itself.
///
/// On Windows a Job Object goes on listing a process for a moment after
/// the process has exited. Asked in that moment, a count of the job's
/// members said that the command had left one process running: on a hosted
/// runner, for 18 to 62 of 120 commands. The shell tool then waited half a
/// second for a process that was not there. The question is put the
/// instant the command has exited, many times over, because the moment is
/// short.
#[test]
fn sys_a_command_that_left_nothing_is_never_counted_itself() {
    for round in 0..40 {
        let mut command = if cfg!(windows) {
            let mut command = std::process::Command::new("cmd.exe");
            command.args(["/d", "/c", "echo hi"]);
            command
        } else {
            let mut command = std::process::Command::new("sh");
            command.args(["-c", "echo hi"]);
            command
        };
        command.stdin(std::process::Stdio::null());
        let (mut output, _stop) = Output::attach(&mut command).expect("make the pipe");
        let tree = Tree::spawn(command, None).expect("start the command");
        assert_eq!(tree.wait().expect("wait"), Exit::Code(0));
        assert_eq!(
            tree.others_running().expect("count"),
            0,
            "round {round}: a command that had exited was counted as left running"
        );
        assert_eq!(read_all(&mut output).trim_end(), "hi");
        tree.end();
    }
}

#[test]
fn sys_dropping_the_last_handle_ends_the_tree() {
    let roll = Roll::new();
    let command = ChildSpec::new(&roll, "parent")
        .with_child(ChildSpec::new(&roll, "grandchild").stubborn())
        .command(ENTRY);
    let tree = Tree::spawn(command, None).expect("start the command");
    let both = roll.all(&["parent", "grandchild"], LIMIT);

    let second = tree.clone();
    drop(tree);
    assert!(both[0].is_alive(), "a clone keeps the tree");
    drop(second);
    assert!(both[0].gone_within(LIMIT));
    assert!(both[1].gone_within(LIMIT));
}

#[test]
fn sys_a_read_of_the_output_can_be_stopped() {
    let roll = Roll::new();
    // The child stays, so the pipe stays open and a read of it blocks.
    let mut command = ChildSpec::new(&roll, "holder")
        .saying("first")
        .command(ENTRY);
    let (mut output, stop) = Output::attach(&mut command).expect("make the pipe");
    let tree = Tree::spawn(command, None).expect("start the command");
    let holder = roll.next(LIMIT);

    let reader = std::thread::spawn(move || {
        let mut seen = Vec::new();
        let mut buffer = [0u8; 256];
        loop {
            match output.read(&mut buffer).expect("read the output") {
                ReadOutcome::Bytes(count) => seen.extend_from_slice(&buffer[..count]),
                other => return (String::from_utf8_lossy(&seen).into_owned(), other, output),
            }
        }
    });
    // Whether the reader is already blocked or not yet, it stops. A read
    // that was just starting when it was told may need telling again.
    cortex_testkit::children::until(LIMIT, "the reader to stop", || {
        stop.stop();
        reader.is_finished()
    });
    let (_seen, outcome, mut output) = reader.join().expect("the reader ended");
    assert_eq!(outcome, ReadOutcome::Stopped);
    assert!(holder.is_alive(), "stopping a read stops nothing else");
    // And it stays stopped.
    let mut buffer = [0u8; 16];
    assert_eq!(
        output.read(&mut buffer).expect("read again"),
        ReadOutcome::Stopped
    );
    tree.end();
    assert!(holder.gone_within(LIMIT));
}

#[test]
fn sys_a_tree_does_not_outlive_a_harness_that_is_killed() {
    let roll = Roll::new();
    let child =
        ChildSpec::new(&roll, "parent").with_child(ChildSpec::new(&roll, "grandchild").stubborn());
    let mut harness = harness(HARNESS, &child).spawn().expect("start the harness");
    let both = roll.all(&["parent", "grandchild"], LIMIT);

    // No warning and no clean-up: the harness is simply gone.
    harness.kill().expect("kill the harness");
    harness.wait().expect("reap the harness");

    assert!(
        both[0].gone_within(LIMIT),
        "the command outlived the harness"
    );
    assert!(
        both[1].gone_within(LIMIT),
        "the grandchild outlived the harness"
    );
}

/// Without the guard, Linux still kills the command itself: the kernel's
/// death signal. On Windows the Job Object needs no guard at all.
#[test]
fn sys_the_command_itself_dies_with_the_harness_even_without_the_guard() {
    let roll = Roll::new();
    let child = ChildSpec::new(&roll, "alone");
    let mut harness = harness(BARE_HARNESS, &child)
        .spawn()
        .expect("start the harness");
    let alone = roll.next(LIMIT);

    harness.kill().expect("kill the harness");
    harness.wait().expect("reap the harness");
    assert!(alone.gone_within(LIMIT), "the command outlived the harness");
}

/// The guard is told when a tree ends, so that it never kills a group
/// whose id the system has since given to something else. And a guard
/// that has died is replaced, and told of every tree still running.
#[cfg(target_os = "linux")]
#[test]
fn sys_a_guard_that_died_is_started_again() {
    use cortex_testkit::children::until;

    use crate::guard;

    let roll = Roll::new();
    let guard = guard();
    let first = Tree::spawn(ChildSpec::new(&roll, "first").command(ENTRY), Some(&guard))
        .expect("start the first tree");
    let first_child = roll.next(LIMIT);
    first.end();
    assert!(first_child.gone_within(LIMIT));

    let second = Tree::spawn(ChildSpec::new(&roll, "second").command(ENTRY), Some(&guard))
        .expect("start the second tree");
    let second_child = roll.next(LIMIT);

    // Kill the guard's process, and wait until it is dead.
    let old = guard_processes();
    assert_eq!(old.len(), 1, "one guard serves every tree");
    let status = std::process::Command::new("kill")
        .args(["-KILL", &old[0].to_string()])
        .status()
        .expect("run kill");
    assert!(status.success());
    until(LIMIT, "the guard's process to die", || {
        guard_processes().is_empty()
    });
    assert!(second_child.is_alive(), "a dead guard kills nothing");

    // The next tree finds the guard gone and starts another.
    let third = Tree::spawn(ChildSpec::new(&roll, "third").command(ENTRY), Some(&guard))
        .expect("start the third tree, with a new guard");
    let third_child = roll.next(LIMIT);
    // The new guard's process may not be in the table the instant `spawn`
    // returns under load, so wait for it before counting.
    until(LIMIT, "the new guard's process to appear", || {
        guard_processes().len() == 1
    });
    let new = guard_processes();
    assert_eq!(new.len(), 1);
    assert_ne!(new, old);
    assert!(second_child.is_alive() && third_child.is_alive());

    second.end();
    third.end();
    assert!(second_child.gone_within(LIMIT) && third_child.gone_within(LIMIT));
}

/// The live guard processes this test's process started.
#[cfg(target_os = "linux")]
fn guard_processes() -> Vec<u32> {
    guards_of(std::process::id())
}

/// The live guard processes that are children of `parent`: those that run
/// this binary as the guard and have not exited.
#[cfg(target_os = "linux")]
fn guards_of(parent: u32) -> Vec<u32> {
    let parent = parent.to_string();
    let mut found = Vec::new();
    for entry in std::fs::read_dir("/proc").expect("read /proc") {
        let entry = entry.expect("an entry of /proc");
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        let Ok(command_line) = std::fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        // After the name, which is in brackets: state, parent, group, session.
        let Some((_, rest)) = stat.rsplit_once(") ") else {
            continue;
        };
        let fields: Vec<&str> = rest.split(' ').collect();
        let is_live_child = fields.first() != Some(&"Z") && fields.get(1) == Some(&parent.as_str());
        let is_guard = command_line
            .split(|byte| *byte == 0)
            .any(|argument| argument == crate::GUARD.as_bytes());
        if is_live_child && is_guard {
            found.push(pid);
        }
    }
    found
}

/// Sends a signal with the system's `kill` program: to a process, or with
/// a negative number to a process group.
#[cfg(unix)]
fn send(signal: &str, target: i64) {
    let status = std::process::Command::new("kill")
        .args([&format!("-{signal}"), "--", &target.to_string()])
        .status()
        .expect("run kill");
    assert!(status.success(), "kill -{signal} {target} failed");
}

/// The guard itself, with nothing around it: it kills the groups it was
/// told of and not told to forget, when its pipe closes, and nothing else.
#[cfg(unix)]
#[test]
fn sys_the_guard_kills_what_it_was_told_and_nothing_else() {
    use std::io::Write;

    let roll = Roll::new();
    let spawn = |name: &str| {
        Tree::spawn(
            ChildSpec::new(&roll, name)
                .with_child(ChildSpec::new(&roll, &format!("{name}-child")))
                .command(ENTRY),
            None,
        )
        .expect("start a tree")
    };
    let (told, forgotten, never_told) = (spawn("told"), spawn("forgotten"), spawn("never-told"));
    let (announced, taken_back) = (spawn("announced"), spawn("taken-back"));
    let all = roll.all(
        &[
            "told",
            "told-child",
            "announced",
            "announced-child",
            "forgotten",
            "forgotten-child",
            "never-told",
            "never-told-child",
            "taken-back",
            "taken-back-child",
        ],
        LIMIT,
    );

    let mut guard = std::process::Command::new(std::env::current_exe().expect("a path"))
        .args(["--exact", ENTRY, "--nocapture", crate::GUARD])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("start the guard");
    let mut pipe = guard.stdin.take().expect("the guard's pipe");
    // A tree that ended, a tree that runs, and ids no tree has: everyone,
    // the caller's own group, the first process, and nonsense.
    // And one that only announced itself, as when the harness dies before
    // it can confirm, after one whose start was taken back.
    write!(
        pipe,
        "?{taken_back}\n!\n+{forgotten}\n+{told}\n-{forgotten}\n?{announced}\n\
         +0\n+1\n+-1\n?0\nkill everything\n",
        taken_back = taken_back.id(),
        forgotten = forgotten.id(),
        told = told.id(),
        announced = announced.id(),
    )
    .expect("tell the guard");
    pipe.flush().expect("tell the guard");
    // Told, it does nothing while the pipe is open.
    assert!(all.iter().all(cortex_testkit::children::Present::is_alive));

    // The harness is gone: its end of the pipe closes.
    drop(pipe);
    assert!(guard.wait().expect("the guard ends").success());
    for killed in &all[..4] {
        assert!(killed.gone_within(LIMIT), "`{}` survived", killed.name());
    }
    for other in &all[4..] {
        assert!(other.is_alive(), "it killed `{}`", other.name());
    }
    for tree in [forgotten, never_told, told, announced, taken_back] {
        tree.end();
    }
}

/// The harness's side of the pipe: the guard is told of a tree when it
/// starts and told to forget it when it ends, in that order, so that the
/// guard never holds the id of a group that is gone.
#[cfg(unix)]
#[test]
fn sys_the_guard_is_told_of_each_tree_and_of_its_end() {
    use cortex_sandbox::tree::Guard;
    use cortex_testkit::children::until;

    let directory =
        std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("cortex-sandbox-it-guard-told");
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("create a directory");
    let record = directory.join("told.txt");
    // In place of the guard, a program that writes down what it is told.
    let guard = Guard::new("sh", ["-c", "cat > \"$0\"", record.to_str().expect("text")]);
    let told = || std::fs::read_to_string(&record).unwrap_or_default();

    let roll = Roll::new();
    let first = Tree::spawn(ChildSpec::new(&roll, "first").command(ENTRY), Some(&guard))
        .expect("start the first tree");
    let second = Tree::spawn(ChildSpec::new(&roll, "second").command(ENTRY), Some(&guard))
        .expect("start the second tree");
    let both = roll.all(&["first", "second"], LIMIT);
    let (a, b) = (first.id(), second.id());
    // Each tree twice: by its own first process before that became the
    // command, and by the harness once the command was started.
    let starts = format!("?{a}\n+{a}\n?{b}\n+{b}\n");
    until(LIMIT, "the guard to be told of both trees", || {
        told() == starts
    });

    first.end();
    until(LIMIT, "the guard to be told that a tree ended", || {
        told() == format!("{starts}-{a}\n")
    });
    // Ending it again says nothing more.
    first.end();
    drop(second);
    assert!(both[0].gone_within(LIMIT) && both[1].gone_within(LIMIT));
    until(LIMIT, "the guard to be told that the other ended", || {
        told() == format!("{starts}-{a}\n-{b}\n")
    });
    // The guard's pipe closes with its last handle, which a tree holds
    // too, and the stand-in ends having been told nothing more.
    drop(first);
    drop(guard);
    assert_eq!(told(), format!("{starts}-{a}\n-{b}\n"));
    let _ = std::fs::remove_dir_all(&directory);
}

/// A command that cannot be started may have told the guard of itself
/// before it failed. The harness takes that back, so that the guard is
/// not left holding the id of a process that is gone.
#[cfg(unix)]
#[test]
fn sys_a_start_that_fails_is_taken_back_from_the_guard() {
    use cortex_sandbox::tree::Guard;
    use cortex_testkit::children::until;

    let directory =
        std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("cortex-sandbox-it-guard-failed");
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("create a directory");
    let record = directory.join("told.txt");
    let guard = Guard::new("sh", ["-c", "cat > \"$0\"", record.to_str().expect("text")]);
    let told = || std::fs::read_to_string(&record).unwrap_or_default();

    let missing = std::process::Command::new(directory.join("no-such-program"));
    let error = Tree::spawn(missing, Some(&guard)).expect_err("there is no such program");
    assert_eq!(error.kind(), io::ErrorKind::NotFound, "{error}");
    until(LIMIT, "the guard to be told that the start failed", || {
        told().ends_with("!\n")
    });
    // What announced itself, and then the taking back: nothing else.
    let lines: Vec<String> = told().lines().map(str::to_owned).collect();
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(
        lines[0].starts_with('?') && lines[0][1..].bytes().all(|byte| byte.is_ascii_digit()),
        "{lines:?}"
    );
    assert_eq!(lines[1], "!");

    // The guard goes on serving the next tree.
    let roll = Roll::new();
    let tree = Tree::spawn(ChildSpec::new(&roll, "next").command(ENTRY), Some(&guard))
        .expect("start a tree");
    let next = roll.next(LIMIT);
    let id = tree.id();
    until(LIMIT, "the guard to be told of the next tree", || {
        told().ends_with(&format!("!\n?{id}\n+{id}\n"))
    });
    tree.end();
    assert!(next.gone_within(LIMIT));
    drop(tree);
    drop(guard);
    let _ = std::fs::remove_dir_all(&directory);
}

/// The guard is not in the harness's process group, so what kills that
/// group does not kill the guard: a terminal that closes signals a whole
/// group, and so does a kill by a job-control shell.
#[cfg(unix)]
#[test]
fn sys_the_guard_outlives_a_kill_of_the_harnesss_whole_group() {
    use std::os::unix::process::CommandExt;

    let roll = Roll::new();
    let child =
        ChildSpec::new(&roll, "parent").with_child(ChildSpec::new(&roll, "grandchild").stubborn());
    // The harness leads a process group of its own, like a job of a shell.
    let mut harness = harness(HARNESS, &child)
        .process_group(0)
        .spawn()
        .expect("start the harness");
    let both = roll.all(&["parent", "grandchild"], LIMIT);

    send("KILL", -i64::from(harness.id()));
    harness.wait().expect("reap the harness");
    assert!(
        both[0].gone_within(LIMIT),
        "the command outlived the harness"
    );
    assert!(
        both[1].gone_within(LIMIT),
        "the grandchild outlived the harness: the guard died with its group"
    );
}

/// The guard ignores the signals a harness is stopped with, and has a
/// name of its own, so that a kill of everything named like the harness
/// leaves it to do its work.
#[cfg(target_os = "linux")]
#[test]
fn sys_the_guard_ignores_what_stops_a_harness_and_has_its_own_name() {
    use cortex_testkit::children::until;

    let roll = Roll::new();
    let child =
        ChildSpec::new(&roll, "parent").with_child(ChildSpec::new(&roll, "grandchild").stubborn());
    let mut harness = harness(HARNESS, &child).spawn().expect("start the harness");
    let both = roll.all(&["parent", "grandchild"], LIMIT);
    let guards = guards_of(harness.id());
    assert_eq!(guards.len(), 1, "a harness has one guard");
    let guard = guards[0];
    let name = || std::fs::read_to_string(format!("/proc/{guard}/comm")).unwrap_or_default();
    // It names itself, and stops listening to these signals, as it starts.
    until(LIMIT, "the guard to name itself", || {
        name().trim() == "cx-tree-guard"
    });

    for signal in ["TERM", "INT", "HUP", "QUIT"] {
        send(signal, i64::from(guard));
    }
    // A signal takes effect when the guard next runs, which may be after
    // this line. What shows that none of them stopped it is below: only a
    // live guard can kill the grandchild.
    assert!(both[0].is_alive() && both[1].is_alive());

    harness.kill().expect("kill the harness");
    harness.wait().expect("reap the harness");
    assert!(both[0].gone_within(LIMIT));
    assert!(
        both[1].gone_within(LIMIT),
        "the grandchild outlived the harness: a signal had stopped the guard"
    );
}

/// A harness that was started with `SIGCHLD` ignored would have its
/// children reaped by the system as they exit, and their ids given away.
/// Starting a tree puts that right.
#[cfg(unix)]
#[test]
fn sys_a_harness_that_ignores_its_childrens_exit_is_put_right() {
    let roll = Roll::new();
    let child = ChildSpec::new(&roll, "counted").exiting(3);
    // An ignored signal stays ignored across `exec`.
    let status = crate::harness_through(
        &["sh", "-c", "trap '' CHLD; exec \"$0\" \"$@\""],
        crate::WAITER,
        &child,
    )
    .status()
    .expect("run the waiter");
    assert_eq!(
        status.code(),
        Some(crate::WAITED + 3),
        "the command's exit was lost"
    );
}

/// The command leads a session and a process group of its own, so it has
/// no controlling terminal and one signal reaches all of it.
#[cfg(target_os = "linux")]
#[test]
fn sys_a_command_leads_its_own_session_and_group() {
    let roll = Roll::new();
    let tree = Tree::spawn(ChildSpec::new(&roll, "leader").command(ENTRY), None)
        .expect("start the command");
    let leader = roll.next(LIMIT);
    let stat = std::fs::read_to_string(format!("/proc/{}/stat", tree.id())).expect("read stat");
    let (_, rest) = stat.rsplit_once(") ").expect("the fields after the name");
    let fields: Vec<&str> = rest.split(' ').collect();
    // state, parent, group, session, terminal.
    assert_eq!(fields[1], std::process::id().to_string(), "its parent");
    assert_eq!(fields[2], tree.id().to_string(), "its process group");
    assert_eq!(fields[3], tree.id().to_string(), "its session");
    assert_eq!(fields[4], "0", "its controlling terminal");
    tree.end();
    assert!(leader.gone_within(LIMIT));
}

#[cfg(windows)]
#[test]
fn sys_a_tree_is_held_to_the_standard_limits() {
    use cortex_sandbox::tree::Limits;
    let mut command = std::process::Command::new("cmd");
    command.args(["/d", "/c", "exit 0"]);
    let tree = Tree::spawn(command, None).expect("start the command");
    let limits = tree.limits().expect("ask the job for its limits");
    assert_eq!(limits.processes, Some(Limits::PROCESSES));
    assert!(limits.memory.is_some_and(|bytes| bytes > 0), "{limits:?}");
    assert_eq!(limits, Limits::standard());
    tree.wait().expect("wait");
    tree.end();
}

#[cfg(windows)]
#[test]
fn sys_a_tree_cannot_start_a_process_past_its_limit() {
    use cortex_sandbox::tree::Limits;
    for (processes, inner) in [(1, false), (2, true)] {
        let mut command = std::process::Command::new("cmd");
        command.args(["/d", "/c", "echo outer-ran & cmd /d /c echo inner-ran"]);
        let (mut output, _stop) = Output::attach(&mut command).expect("make the pipe");
        let limits = Limits {
            processes: Some(processes),
            memory: None,
        };
        let tree = Tree::spawn_limited(command, None, limits).expect("start the command");
        let text = read_all(&mut output);
        tree.wait().expect("wait");
        tree.end();
        assert!(text.contains("outer-ran"), "{processes}: {text:?}");
        assert_eq!(text.contains("inner-ran"), inner, "{processes}: {text:?}");
    }
}

#[cfg(target_os = "linux")]
#[test]
fn sys_a_process_that_makes_a_group_of_its_own_is_still_killed_with_the_tree() {
    let exe = std::env::current_exe().expect("the test binary has a path");
    let mut command = std::process::Command::new("/bin/sh");
    command
        .arg("-c")
        .arg(format!(
            "\"$0\" --exact {ENTRY} --nocapture {} own-group & exit 0",
            crate::CALL
        ))
        .arg(&exe);
    let (mut output, _stop) = Output::attach(&mut command).expect("make the pipe");
    let tree = Tree::spawn(command, None).expect("start the command");
    // The helper says its id once it is in a group of its own.
    let mut text = String::new();
    let mut buffer = [0u8; 4096];
    let started = std::time::Instant::now();
    while !text.lines().any(|line| line.starts_with("group: ")) {
        assert!(
            started.elapsed() < LIMIT,
            "the helper said nothing: {text:?}"
        );
        match output.read(&mut buffer).expect("read the output") {
            ReadOutcome::Bytes(count) => text.push_str(&String::from_utf8_lossy(&buffer[..count])),
            other => panic!("the output ended: {other:?} {text:?}"),
        }
    }
    let line = text
        .lines()
        .find_map(|line| line.strip_prefix("group: "))
        .expect("the line");
    let (pid, made) = line.split_once(' ').expect("an id and a result");
    assert_eq!(made, "0", "setpgid: {line}");
    assert_eq!(tree.wait().expect("wait"), Exit::Code(0));
    // Outside the command's group, inside its session: still counted.
    assert!(tree.others_running().expect("count") >= 1);
    tree.end();
    let gone = || {
        std::fs::read_to_string(format!("/proc/{pid}/stat")).map_or(true, |stat| {
            stat.rsplit_once(") ")
                .is_some_and(|(_, rest)| rest.starts_with('Z'))
        })
    };
    let started = std::time::Instant::now();
    while !gone() {
        assert!(
            started.elapsed() < LIMIT,
            "the process in its own group outlived the tree"
        );
        std::thread::yield_now();
    }
}

/// A turn the user is pulling out of does not wait on the kill's sweep for
/// what a command left running: `end_until` with a raised interrupt stops the
/// sweep and reports the tree as escaped, so the continuity check covers it.
/// The whole tree is still signalled to die first, so nothing is left alive
/// by the early return (found by the independent review of the step 26
/// fixes). Tested with a token that is already cancelled, so no real time
/// passes.
#[cfg(target_os = "linux")]
#[test]
fn sys_a_raised_interrupt_stops_the_kill_sweep() {
    let roll = Roll::new();
    let command = ChildSpec::new(&roll, "parent")
        .exiting(0)
        .with_child(ChildSpec::new(&roll, "left-behind").stubborn())
        .command(ENTRY);
    let tree = Tree::spawn(command, None).expect("start the command");
    let both = roll.all(&["parent", "left-behind"], LIMIT);
    assert_eq!(tree.wait().expect("wait"), Exit::Code(0));
    assert!(both[1].is_alive());

    let cancel = cortex_base::CancelToken::new();
    cancel.cancel();
    tree.end_until(&cancel);
    assert!(
        tree.escaped(),
        "a cancelled end reports the sweep did not confirm the session clear"
    );
    // The tree was signalled to die all the same: the left-behind process is
    // gone, the sweep only stopped waiting to confirm it.
    assert!(both[1].gone_within(LIMIT), "the tree was not killed");
}
