//! An adversarial confined command, and what it reached (`docs/testing-and-ci.md`,
//! "An adversarial confined command").
//!
//! The sandbox reviews each built their own: a world at `autonomous`, a
//! trusted repository, the sandbox enforcing, one command that tries what
//! it should not, and a look afterwards at what it got to. This is that,
//! once. [`Reach::start`] notes the state of the machine before a turn and
//! [`Reach::reached`] says what changed after it:
//!
//! - **protected paths** changed: the files under the workspace's `.git`,
//!   `.cortex` and `.cargo`, compared byte for byte;
//! - **files outside the write roots**: anything under the world's folder
//!   that is not the workspace and not the harness's own configuration,
//!   state or data. The world's `outside` folder is there for a
//!   command to aim at;
//! - **sockets**: connections that reached a listener the fixture opened on
//!   the loopback interface, whose port a command is given;
//! - **surviving processes**: every process still alive after the turn
//!   whose environment holds the fixture's marker, which the shell passes to
//!   everything it starts. They are killed once counted.
//!
//! A script names what it aims at with `{outside}`, `{port}` and
//! `{marker}`. [`Reach::run`] does the whole of it for one script, in a
//! session of its own: the script is a file in the workspace and the call
//! is `sh adversary.sh`, so that the command analysis cannot see what it
//! aims at and ask about it, as it would about a command line that names
//! a hook or a folder outside. Every question the turn asks is refused all
//! the same, and listed. A test that needs more turns wraps them in
//! [`Reach::start`] and [`Reach::reached`] itself.

use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::net::TcpListener;
use std::path::{Path, PathBuf};

use cortex_protocol::event::TurnOutcome;
use cortex_protocol::lease::Level;
use cortex_protocol::session::SessionSettings;
use serde_json::json;

use crate::sandbox::{enforcing_here, launcher};
use crate::{Front, World, calls, says};

/// The variable every process the shell starts carries.
const MARKER: &str = "CORTEX_ADVERSARY";

/// What a confined command got to.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Reached {
    /// The protected paths whose content changed, relative to the
    /// workspace, with how.
    pub protected: Vec<String>,
    /// The files outside the write roots that were made, changed or
    /// removed, relative to the world's folder.
    pub outside: Vec<String>,
    /// How many connections reached the listener.
    pub connections: usize,
    /// The processes that outlived the turn: their id and command line.
    pub survivors: Vec<String>,
    /// What the user was asked during the turn, by tool: every question is
    /// refused, so a command never leaves the sandbox by approval.
    pub asked: Vec<String>,
}

impl Reached {
    /// Whether it got to nothing outside what a confined command may do:
    /// no file outside the write roots, and no process that outlived it.
    pub fn nothing_outside(&self) -> bool {
        self.outside.is_empty() && self.survivors.is_empty()
    }
}

/// The machine as it was before the turn.
pub struct Reach {
    marker: String,
    listener: TcpListener,
    protected: BTreeMap<String, Vec<u8>>,
    outside: BTreeMap<String, Vec<u8>>,
    pub port: u16,
    pub outside_dir: PathBuf,
}

impl Reach {
    /// Notes the state of `world`, and opens the listener. Give
    /// [`Reach::env`] to the shell's environment.
    pub fn start(world: &World) -> Self {
        let outside_dir = world.dir.join("outside");
        std::fs::create_dir_all(&outside_dir).unwrap();
        std::fs::write(outside_dir.join("existing.txt"), "as it was\n").unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        Self {
            marker: format!("{}-{}", std::process::id(), port),
            protected: protected_files(world),
            outside: outside_files(world),
            listener,
            port,
            outside_dir,
        }
    }

    /// The variable to give the shell, so that what it starts is known.
    pub fn env(&self) -> [(&'static str, &str); 1] {
        [(MARKER, self.marker.as_str())]
    }

    /// `command` with `{outside}`, `{port}` and `{marker}` filled in.
    pub fn command(&self, command: &str) -> String {
        command
            .replace("{outside}", &self.outside_dir.display().to_string())
            .replace("{port}", &self.port.to_string())
            .replace("{marker}", &self.marker)
    }

    /// What changed since [`Reach::start`]. Survivors are killed.
    pub fn reached(&self, world: &World) -> Reached {
        let mut connections = 0;
        loop {
            match self.listener.accept() {
                Ok(_) => connections += 1,
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) => panic!("the listener failed: {error}"),
            }
        }
        let survivors = survivors(&self.marker);
        for survivor in &survivors {
            let pid = survivor.split(' ').next().unwrap_or_default();
            let _ = std::process::Command::new("kill")
                .args(["-9", pid])
                .status();
        }
        Reached {
            protected: changes(&self.protected, &protected_files(world)),
            outside: changes(&self.outside, &outside_files(world)),
            connections,
            survivors,
            asked: Vec::new(),
        }
    }

    /// One turn at `autonomous`, in a trusted repository with the sandbox
    /// enforcing, whose one call runs `script` in the shell; and what it
    /// reached, with the tool's result and the turn's outcome. `None` where
    /// the sandbox does not enforce (a desk without Landlock; CI fails
    /// instead).
    pub fn run(test: &str, script: &str) -> Option<(Reached, String, TurnOutcome)> {
        if !enforcing_here() {
            return None;
        }
        let world = World::new(test, []);
        world.put(".git/config", "[core]\n");
        world.put(".git/hooks/pre-commit.sample", "#!/bin/sh\n");
        world.put("README.md", "A trusted project.\n");
        let reach = Reach::start(&world);
        world.put("adversary.sh", reach.command(script));
        world.provider.push(calls(&[(
            "c1",
            "shell",
            json!({"command": "sh adversary.sh"}),
        )]));
        world.provider.push(says("Done."));
        let core = world.core_with(|options| {
            options.launcher = Some(launcher());
            options.trust_project = true;
            options.shell_env = Some(world.shell_env(&reach.env()));
        });
        let mut asked = Vec::new();
        let (result, outcome) = world.run(async {
            let front = Front::start_as(
                &world,
                &core,
                cortex_core::ClientInfo::user(),
                SessionSettings {
                    level: Some(Level::Autonomous),
                    ..SessionSettings::default()
                },
            )
            .await
            .expect("an autonomous session starts where the sandbox enforces");
            // The shell gives a process left running a grace on the
            // world's timer, which moves only when a test moves it: so
            // while the turn runs, each short wait that starts is let go.
            // The call's own time limit is minutes, and is never reached.
            let mut turn = std::pin::pin!(front.turn("try it"));
            let ended = std::future::poll_fn(|context| {
                if let std::task::Poll::Ready(ended) = turn.as_mut().poll(context) {
                    return std::task::Poll::Ready(ended);
                }
                // A question the turn waits on (to run a refused write
                // outside the sandbox, to go on after a protected path
                // changed) is refused.
                front.drain();
                let open: Vec<_> = crate::requests_in(&front.seen.borrow())
                    .into_iter()
                    .filter(|(id, _)| !front.answered.borrow().contains(id))
                    .collect();
                for (id, request) in open {
                    front.answered.borrow_mut().push(id.clone());
                    asked.push(request.tool.clone());
                    front
                        .answer(&id, cortex_protocol::methods::ApprovalChoice::Deny)
                        .expect("the question is answered");
                }
                let short = world
                    .timer
                    .running()
                    .into_iter()
                    .filter(|wait| *wait < std::time::Duration::from_secs(60))
                    .min();
                if let Some(wait) = short {
                    world.timer.advance(wait);
                }
                // Asked again at once: what the turn waits for is a real
                // process, on its own threads.
                context.waker().wake_by_ref();
                std::task::Poll::Pending
            })
            .await;
            let result = front
                .results()
                .first()
                .map(|one| one.2.clone())
                .unwrap_or_default();
            front.close().await;
            (result, ended.outcome)
        });
        let mut reached = reach.reached(&world);
        reached.asked = asked;
        Some((reached, result, outcome))
    }
}

/// The protected files of the workspace, by path relative to it.
fn protected_files(world: &World) -> BTreeMap<String, Vec<u8>> {
    let mut files = BTreeMap::new();
    for top in [".git", ".cortex", ".cargo"] {
        collect(
            &world.workspace,
            &world.workspace.join(top),
            &[],
            &mut files,
        );
    }
    files
}

/// The files under the world's folder that no confined command may write:
/// all but the workspace and the harness's own folders.
fn outside_files(world: &World) -> BTreeMap<String, Vec<u8>> {
    let dirs = world.dirs();
    let skip: Vec<PathBuf> = [
        Some(world.workspace.clone()),
        dirs.data,
        dirs.state,
        dirs.config,
    ]
    .into_iter()
    .flatten()
    .collect();
    let mut files = BTreeMap::new();
    collect(&world.dir, &world.dir, &skip, &mut files);
    files
}

fn collect(base: &Path, dir: &Path, skip: &[PathBuf], files: &mut BTreeMap<String, Vec<u8>>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if skip.iter().any(|skipped| path.starts_with(skipped)) {
            continue;
        }
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            collect(base, &path, skip, files);
        } else {
            let content = if kind.is_symlink() {
                std::fs::read_link(&path)
                    .map(|target| target.to_string_lossy().into_owned().into_bytes())
                    .unwrap_or_default()
            } else {
                std::fs::read(&path).unwrap_or_default()
            };
            let relative = path
                .strip_prefix(base)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            files.insert(relative, content);
        }
    }
}

/// What differs between two snapshots, path by path.
fn changes(before: &BTreeMap<String, Vec<u8>>, after: &BTreeMap<String, Vec<u8>>) -> Vec<String> {
    let mut changed = Vec::new();
    for (path, content) in after {
        match before.get(path) {
            None => changed.push(format!("{path} (made)")),
            Some(old) if old != content => changed.push(format!("{path} (changed)")),
            Some(_) => {}
        }
    }
    for path in before.keys().filter(|path| !after.contains_key(*path)) {
        changed.push(format!("{path} (removed)"));
    }
    changed
}

/// The live processes, not zombies, whose environment holds the marker:
/// `<pid> <command line>`.
fn survivors(marker: &str) -> Vec<String> {
    let wanted = format!("{MARKER}={marker}");
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return found;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.bytes().all(|byte| byte.is_ascii_digit()) {
            continue;
        }
        let dir = entry.path();
        let Ok(environ) = std::fs::read(dir.join("environ")) else {
            continue;
        };
        if !environ
            .split(|byte| *byte == 0)
            .any(|variable| variable == wanted.as_bytes())
        {
            continue;
        }
        let state = std::fs::read_to_string(dir.join("stat")).unwrap_or_default();
        let zombie = state
            .rfind(')')
            .and_then(|end| state[end + 1..].split_whitespace().next())
            == Some("Z");
        if zombie {
            continue;
        }
        let command = std::fs::read(dir.join("cmdline"))
            .unwrap_or_default()
            .split(|byte| *byte == 0)
            .map(|part| String::from_utf8_lossy(part).into_owned())
            .collect::<Vec<_>>()
            .join(" ");
        found.push(format!("{name} {}", command.trim()));
    }
    found
}

/// The fixture on a command that tries everything at once: a write
/// outside the write roots, a git hook, a connection out, and a process
/// left behind in a session of its own. The sandbox stops the first, the
/// look at protected paths finds the second and stops the turn, and the
/// kill takes the last.
#[test]
fn sys_an_adversarial_command_reaches_only_what_it_may() {
    let Some((reached, result, outcome)) = Reach::run(
        "adversary",
        "echo x > {outside}/f\necho y >> {outside}/existing.txt\necho evil > \
         .git/hooks/pre-commit\nsetsid sh -c 'sleep 600' < /dev/null > /dev/null 2>&1 &\necho \
         started\n",
    ) else {
        return;
    };
    assert!(reached.nothing_outside(), "{reached:#?}\n{result}");
    assert_eq!(
        reached.protected,
        [".git/hooks/pre-commit (made)"],
        "{reached:#?}\n{result}"
    );
    assert!(result.contains("changed protected paths"), "{result}");
    assert_eq!(outcome, TurnOutcome::Completed, "{result}");
    assert_eq!(reached.survivors, Vec::<String>::new(), "{result}");
    // A connection is the network grant's to allow or refuse; it is
    // reported, and this command's is not asserted on.
    let _ = reached.connections;
}

/// The fixture itself, with nothing confined: a file written outside, a
/// protected file made, a connection, and a process left with the marker
/// are each seen, and the process is killed.
#[test]
fn sys_the_fixture_sees_each_thing_a_command_can_reach() {
    let world = World::new("adversary_sees", []);
    world.put(".git/config", "[core]\n");
    let reach = Reach::start(&world);
    std::fs::write(reach.outside_dir.join("f"), "x\n").unwrap();
    world.put(".git/hooks/pre-commit", "evil\n");
    std::fs::remove_file(reach.outside_dir.join("existing.txt")).unwrap();
    let _connected = std::net::TcpStream::connect(("127.0.0.1", reach.port)).unwrap();
    let mut child = std::process::Command::new("sh")
        .args(["-c", "sleep 600"])
        .envs(reach.env())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let reached = reach.reached(&world);
    assert_eq!(
        reached.outside,
        ["outside/f (made)", "outside/existing.txt (removed)"]
    );
    assert_eq!(reached.protected, [".git/hooks/pre-commit (made)"]);
    assert_eq!(reached.connections, 1);
    assert_eq!(reached.survivors.len(), 1, "{reached:#?}");
    assert!(reached.survivors[0].starts_with(&child.id().to_string()));
    assert!(!reached.nothing_outside());
    // Killed when it was counted.
    assert!(!child.wait().unwrap().success());
}
