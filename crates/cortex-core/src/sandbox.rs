//! The sandbox as a session sees it (`docs/architecture/06-permissions-and-sandbox.md`,
//! "The sandbox"): whether it enforces, the launcher the shell tool starts
//! commands through, and the session's private temporary directory.
//!
//! - **The state** comes from what the kernel offers
//!   ([`cortex_sandbox::support`]), whether the binary gave the core a
//!   launcher to start ([`CoreOptions::launcher`](crate::core::CoreOptions::launcher)),
//!   and `sandbox.enabled`. It is what lets `autonomous` start, what
//!   `SessionInfo::sandbox` and the `initialize` result report, and what an
//!   approval's `outside_sandbox` says.
//! - **The launcher** confines every command at every level but
//!   `unrestricted`, where commands run as they are. It is given the
//!   lease's write roots and network setting, the session's temporary
//!   directory, and the Landlock version detection found.
//! - **The temporary directory** is made, open to the user alone, when the
//!   session starts, given to every confined command as `TMPDIR`, and
//!   removed when the session ends.
//! - **The harness's own git** runs inside the sandbox too while commands
//!   are confined, with no write root and no network ([`SessionGit`]): a
//!   confined command may write the repository's configuration, which
//!   can name a program for git to run (found by the third review of step
//!   26).

#![allow(
    clippy::disallowed_methods,
    reason = "the temporary directory is made while a session is assembled and removed when it \
              ends, both on the blocking pool"
)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, PoisonError};

use cortex_base::git::{Confinement, Started};

use cortex_policy::{NoSandbox, SandboxState};
use cortex_protocol::SessionId;
use cortex_protocol::lease::{Level, NetworkAccess, SandboxPolicy};
use cortex_protocol::session::SandboxStatus;
use cortex_sandbox::launch::Launcher as SandboxLauncher;
use cortex_sandbox::policy::{Network, Policy, Reach};
use cortex_sandbox::protect::{Changed, Limits, Snapshot, Watch};
use cortex_sandbox::support::{Assessment, Support, Unconfined};
use cortex_tools::process::{Argument, CommandLine, Direct, Exit, Launch, LaunchCx, Launcher};

/// What the core knows of the sandbox, once for its life.
#[derive(Debug, Clone)]
pub(crate) struct CoreSandbox {
    support: Support,
    program: Option<(PathBuf, Vec<OsString>)>,
}

impl CoreSandbox {
    /// From what the system offers, or what a test says it does, and the
    /// program the binary gave to start as the launcher.
    pub(crate) fn new(support: Support, program: Option<(PathBuf, Vec<OsString>)>) -> Self {
        Self { support, program }
    }

    /// What a session with `sandbox.enabled` set as given reports, and the
    /// Landlock version and the gaps it enforces with.
    pub(crate) fn state(&self, enabled: bool) -> (SandboxState, Option<(u8, Vec<&'static str>)>) {
        if !enabled {
            return (SandboxState::NotEnforcing(NoSandbox::TurnedOff), None);
        }
        match self.support.assess() {
            // Without a launcher to start, nothing can be confined.
            Assessment::Enforcing { .. } if self.program.is_none() => {
                (SandboxState::NotEnforcing(NoSandbox::NotBuilt), None)
            }
            Assessment::Enforcing { landlock, gaps } => {
                (SandboxState::Enforcing, Some((landlock, gaps)))
            }
            Assessment::NotEnforcing(why) => (
                SandboxState::NotEnforcing(match why {
                    Unconfined::NotBuilt => NoSandbox::NotBuilt,
                    Unconfined::UnsupportedSystem => NoSandbox::UnsupportedSystem,
                    Unconfined::NoLandlock => NoSandbox::NoLandlock,
                    Unconfined::LandlockTooOld(version) => NoSandbox::LandlockTooOld(version),
                }),
                None,
            ),
        }
    }

    /// The program started as the launcher, as the binary gave it.
    pub(crate) fn program(&self) -> Option<&Path> {
        self.program.as_ref().map(|(program, _)| program.as_path())
    }

    /// What the `initialize` result reports: whether a session with the
    /// sandbox enabled would be confined.
    pub(crate) fn status(&self) -> SandboxStatus {
        status_of(self.state(true).0)
    }
}

/// The protocol's word for a state.
pub(crate) fn status_of(state: SandboxState) -> SandboxStatus {
    match state {
        SandboxState::Enforcing => SandboxStatus::Enforcing,
        SandboxState::NotEnforcing(_) => SandboxStatus::Off,
    }
}

/// Where a session's private temporary directory is.
pub(crate) fn temp_path(session: &SessionId) -> PathBuf {
    std::env::temp_dir().join(format!("cortex-harness-{session}"))
}

/// A session's private temporary directory: where it is, as the lease names
/// it, and whether it was made as the session's own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Temp {
    pub(crate) path: PathBuf,
    pub(crate) made: bool,
}

/// The sandbox of one session: the launcher handed to its shell tool.
#[derive(Debug)]
pub(crate) struct SessionSandbox {
    /// The launcher, when the sandbox enforces.
    launcher: Option<SandboxLauncher>,
    /// The launcher the harness's own git is started through, when the
    /// sandbox enforces: the session's reads, and no write root, no
    /// network and no temporary directory, so that nothing git starts can
    /// change anything.
    reader: Option<SandboxLauncher>,
    /// Whether commands are confined now: the sandbox enforces and the
    /// level is not `unrestricted`.
    confine: AtomicBool,
    /// The private temporary directory.
    temp: Temp,
    /// The protected paths, looked at before and after each confined
    /// command, when the sandbox enforces.
    watch: Option<Watch>,
    /// The look taken before a command that outran its kill (escaped), kept
    /// so that a protected path it changes after its turn ended is found
    /// before the next confined command and at the session's close (found
    /// by the third review of step 26).
    escaped: std::sync::Mutex<Option<Snapshot>>,
}

impl SessionSandbox {
    /// The sandbox of a session in `workspace`, in `state`, at `level`,
    /// whose lease grants `grant`, with its temporary directory, looking at
    /// its protected paths within `limits`.
    #[expect(
        clippy::too_many_arguments,
        reason = "each is one thing a session's sandbox is made from"
    )]
    pub(crate) fn new(
        core: &CoreSandbox,
        state: SandboxState,
        landlock: Option<u8>,
        level: &Level,
        grant: &SandboxPolicy,
        temp: Temp,
        workspace: &Path,
        limits: Limits,
    ) -> Self {
        let (launcher, reader) = match (state, landlock, &core.program) {
            (SandboxState::Enforcing, Some(landlock), Some((program, prefix))) => {
                let policy = policy_of(grant, &temp, landlock);
                let reading = Policy {
                    read: policy.read.clone(),
                    write: Vec::new(),
                    network: Network::Off,
                    temp: None,
                    landlock,
                };
                (
                    Some(SandboxLauncher::new(
                        program.clone(),
                        prefix.clone(),
                        policy,
                    )),
                    Some(SandboxLauncher::new(
                        program.clone(),
                        prefix.clone(),
                        reading,
                    )),
                )
            }
            _ => (None, None),
        };
        let watch = launcher.is_some().then(|| {
            Watch::new(
                workspace,
                grant.protect.roots.iter().map(PathBuf::from).collect(),
            )
            .with_limits(limits)
        });
        let sandbox = Self {
            launcher,
            reader,
            confine: AtomicBool::new(false),
            temp,
            watch,
            escaped: std::sync::Mutex::new(None),
        };
        sandbox.set_level(level);
        sandbox
    }

    /// Remembers the look taken before a command that outran its kill, so
    /// that a protected path it changes after its turn is found later.
    pub(crate) fn remember_escape(&self, before: Snapshot) {
        *self.escaped.lock().unwrap_or_else(PoisonError::into_inner) = Some(before);
    }

    /// If a command escaped its kill, looks again at the protected paths
    /// and says what changed since the look before that command, then
    /// forgets it. Blocks. `None` when none escaped, or nothing changed.
    pub(crate) fn recheck_escape(&self) -> Option<Vec<Changed>> {
        let before = self
            .escaped
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()?;
        let watch = self.watch.as_ref()?;
        let (changed, _) = watch.changes_since(&before);
        (!changed.is_empty()).then_some(changed)
    }

    /// Whether a command that runs now is confined.
    pub(crate) fn confines(&self) -> bool {
        self.confine.load(Ordering::Acquire)
    }

    /// Follows a change of the session's level: nothing is confined at
    /// `unrestricted`.
    pub(crate) fn set_level(&self, level: &Level) {
        let confine = self.launcher.is_some() && *level != Level::Unrestricted;
        self.confine.store(confine, Ordering::Release);
    }

    /// What to look at before and after a command that runs now: the
    /// protected paths, when the command is confined.
    pub(crate) fn watch(&self) -> Option<&Watch> {
        self.watch.as_ref().filter(|_| self.confines())
    }

    /// What to start in place of the harness's own `git` with `arguments`
    /// now: the reading launcher while commands are confined, git as it is
    /// otherwise.
    fn start_git(&self, git: &Path, arguments: Vec<OsString>) -> Started {
        match self.reader.as_ref().filter(|_| self.confines()) {
            Some(reader) => {
                let wrapped = reader.wrap(git.as_os_str(), &arguments);
                Started {
                    program: wrapped.program,
                    arguments: wrapped.arguments,
                    stdin: Some(wrapped.stdin),
                }
            }
            None => Started {
                program: git.to_owned(),
                arguments,
                stdin: None,
            },
        }
    }

    /// Removes the temporary directory and everything in it, if the
    /// session made it. It blocks.
    pub(crate) fn remove_temp(&self) {
        if self.temp.made {
            remove_temp(&self.temp.path);
        }
    }
}

impl Launcher for SessionSandbox {
    fn launch(&self, command: CommandLine, cx: &LaunchCx<'_>) -> Result<Launch, String> {
        let Some(launcher) = self.launcher.as_ref().filter(|_| self.confines()) else {
            return Direct.launch(command, cx);
        };
        let arguments: Vec<OsString> = command
            .arguments
            .iter()
            .map(|argument| match argument {
                Argument::Exact(text) | Argument::Verbatim(text) => OsString::from(text),
            })
            .collect();
        let wrapped = launcher.wrap(command.program.as_os_str(), &arguments);
        let mut line = CommandLine::new(wrapped.program);
        line.arguments = wrapped
            .arguments
            .into_iter()
            .map(|argument| {
                argument
                    .into_string()
                    .map(Argument::Exact)
                    .map_err(|_| "a command line that is not text cannot be confined".to_owned())
            })
            .collect::<Result<_, _>>()?;
        Ok(Launch {
            command: line,
            stdin: Some(wrapped.stdin),
        })
    }

    fn explain(&self, exit: Option<Exit>, output: &str) -> Option<String> {
        let launcher = self.launcher.as_ref().filter(|_| self.confines())?;
        let code = match exit {
            Some(Exit::Code(code)) => Some(code),
            _ => None,
        };
        launcher.explain(code, output)
    }
}

/// Where a session's own git runs: inside its sandbox, reading only,
/// while its commands are confined, and never a git a confined command
/// could have planted inside a write root.
///
/// The session's checkpoints need their git before its sandbox can be
/// made (the sandbox needs the lease), so this is made first and given the
/// sandbox once it is ([`set`](Self::set)). Until then git is not started
/// at all: no answer is better than one from an unconfined git.
#[derive(Debug)]
pub(crate) struct SessionGit {
    sandbox: OnceLock<Arc<SessionSandbox>>,
    /// The resolved write roots to keep the git program out of; empty when
    /// the session would not confine.
    confine_roots: Vec<PathBuf>,
}

impl SessionGit {
    /// A git for a session whose confined commands could write
    /// `confine_roots`: the git the harness starts is never found inside
    /// one of them.
    pub(crate) fn new(confine_roots: Vec<PathBuf>) -> Self {
        Self {
            sandbox: OnceLock::new(),
            confine_roots,
        }
    }

    /// Gives it the session's sandbox. Only the first call counts.
    pub(crate) fn set(&self, sandbox: Arc<SessionSandbox>) {
        let _ = self.sandbox.set(sandbox);
    }
}

impl Confinement for SessionGit {
    fn confines(&self) -> bool {
        self.sandbox.get().is_some_and(|sandbox| sandbox.confines())
    }

    fn git_program(&self) -> Option<PathBuf> {
        if self.confine_roots.is_empty() {
            return cortex_base::program::git().map(Path::to_path_buf);
        }
        cortex_base::program::find_outside(
            &cortex_base::program::path_directories(),
            "git",
            cfg!(windows),
            &self.confine_roots,
        )
    }

    fn start(&self, git: &Path, arguments: Vec<OsString>) -> Result<Started, String> {
        let sandbox = self
            .sandbox
            .get()
            .ok_or_else(|| "the session's sandbox is not made yet".to_owned())?;
        Ok(sandbox.start_git(git, arguments))
    }
}

/// What the model and the user are told after a confined command changed
/// protected paths, or when the look at them stopped at its bound. `None`
/// when there is nothing to say.
pub(crate) fn protected_note(
    changed: &[Changed],
    (complete_before, complete_after): (bool, bool),
    root: &Path,
    by_hand: &[PathBuf],
) -> Option<String> {
    let shown = |paths: &mut dyn Iterator<Item = &Changed>| -> Vec<String> {
        paths
            .map(|changed| {
                let path = changed.path.strip_prefix(root).unwrap_or(&changed.path);
                format!("`{}` ({})", path.display(), changed.change.as_str())
            })
            .collect()
    };
    let mut lines = Vec::new();
    if !changed.is_empty() {
        let all = shown(&mut changed.iter());
        let not_undone = shown(&mut changed.iter().filter(|one| by_hand.contains(&one.path)));
        let undo = if not_undone.is_empty() {
            "Undoing this turn puts them back.".to_owned()
        } else if not_undone.len() == all.len() {
            "Undoing this turn does not put them back: what they were is not kept (a link, a \
             file too large, a change of mode, a directory, a path outside the workspace, or a \
             session that keeps no checkpoints), so they must be put back by hand."
                .to_owned()
        } else {
            format!(
                "Undoing this turn puts back the others; these must be put back by hand: {}.",
                not_undone.join(", ")
            )
        };
        lines.push(format!(
            "[The command changed protected paths, which decide what runs later outside the \
             sandbox: {}. The turn stops here, so that the user can look at the change. {undo}]",
            all.join(", "),
        ));
    }
    if complete_before && !complete_after {
        lines.push(
            "[The look at protected paths after the command stopped at its bound, where the \
             look before it did not: the command made the workspace too large to look at \
             whole, and a change it made further down would not be seen. The turn stops \
             here, so that the user can look.]"
                .to_owned(),
        );
    } else if !complete_after {
        lines.push(
            "[Protected paths were looked at only in part: the look stopped at its bound. A \
             change to one further down the workspace would not have been seen.]"
                .to_owned(),
        );
    }
    (!lines.is_empty()).then(|| lines.join("\n"))
}

/// The launcher's policy, from the lease's grant.
///
/// Each root is resolved to its real path here, once, when the session's
/// policy is made: a link the user set up on purpose (a `sandbox.write`
/// through a symlinked home) is followed now, so that the launcher can
/// open the real path with no link followed and refuse one a confined
/// command plants later (finding 6 of the third review of step 26;
/// [`crate::confine`]'s `grant`). A root that does not resolve (it is not
/// there yet) is kept as it is: the launcher skips a root that is not
/// there, and reaches the no-link rule for one that appears.
fn policy_of(grant: &SandboxPolicy, temp: &Temp, landlock: u8) -> Policy {
    let roots = |roots: &[String]| {
        roots
            .iter()
            .map(|root| resolve_root(Path::new(root)))
            .collect::<Vec<_>>()
    };
    let mut write = roots(&grant.write.roots);
    // The temporary directory is the session's own, made with no link on
    // the way (`make_temp`): its real path is itself.
    let temp_path = resolve_root(&temp.path);
    // A temporary directory that was not made, or is not the session's own,
    // is not written to: someone else could read it, or it leads elsewhere.
    if !temp.made {
        write.retain(|root| *root != temp_path);
    }
    Policy {
        read: if grant.read.all {
            Reach::All
        } else {
            Reach::Only(roots(&grant.read.roots))
        },
        write,
        network: match grant.network {
            NetworkAccess::Off => Network::Off,
            NetworkAccess::Full => Network::Full,
        },
        temp: temp.made.then_some(temp_path),
        landlock,
    }
}

/// A root's real path, with every symbolic link on the way to it resolved,
/// or the path as it is when it cannot be resolved (it is not there). It
/// blocks.
#[allow(
    clippy::disallowed_methods,
    reason = "resolving the roots is part of making the session's sandbox, on the blocking pool"
)]
fn resolve_root(root: &Path) -> PathBuf {
    std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf())
}

/// Makes the session's temporary directory, open to the user alone. It
/// blocks.
///
/// # Errors
///
/// When it cannot be made, or something is already at its path that is not
/// a private directory of the user's own: another user could have put it
/// there, or a link to somewhere else.
pub(crate) fn make_temp(path: &Path) -> std::io::Result<PathBuf> {
    let made = make_private_dir(path);
    match made {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_dir() || !owned_by_me(&metadata) || !cortex_base::private::is_private(path)? {
        return Err(std::io::Error::other(format!(
            "{} is already there and is not a private directory of yours",
            path.display()
        )));
    }
    Ok(path.to_path_buf())
}

#[cfg(unix)]
fn make_private_dir(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new().mode(0o700).create(path)
}

#[cfg(not(unix))]
fn make_private_dir(path: &Path) -> std::io::Result<()> {
    if std::fs::symlink_metadata(path).is_ok() {
        return Err(std::io::ErrorKind::AlreadyExists.into());
    }
    cortex_base::private::create_dir_all(path)
}

#[cfg(unix)]
fn owned_by_me(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    // The owner of a file this process made in the temporary directory is
    // its own: a directory there owned by anyone else was not made here.
    let own = std::fs::metadata("/proc/self").map(|own| own.uid());
    own.is_ok_and(|uid| uid == metadata.uid())
}

#[cfg(not(unix))]
fn owned_by_me(_metadata: &std::fs::Metadata) -> bool {
    // The access-list check of `is_private` says whose it is on Windows.
    true
}

/// Removes a temporary directory and everything in it, following no link.
fn remove_temp(path: &Path) {
    if let Err(error) = std::fs::remove_dir_all(path)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(path = %path.display(), %error, "the session's temporary directory was not removed");
    }
}

#[cfg(test)]
mod tests {
    use cortex_base::Rng;
    use cortex_sandbox::support::Support;

    use super::*;

    /// The harness's own git, while commands are confined, is started by the
    /// launcher under a policy that writes nothing and reaches no network;
    /// at `unrestricted` it is started as it is.
    #[test]
    fn the_harness_git_runs_inside_the_sandbox_reading_only() {
        let workspace = std::env::temp_dir().join("cortex-core-git-policy");
        let root = cortex_base::AbsPath::from_os(&workspace).unwrap();
        let mut grant = cortex_policy::default_sandbox(&root, &[], &[]);
        grant.network = NetworkAccess::Full;
        let core = CoreSandbox::new(
            Support::Landlock(8),
            Some((
                PathBuf::from("/opt/cortex"),
                vec![OsString::from("__sandbox")],
            )),
        );
        let (state, enforcing) = core.state(true);
        let sandbox = Arc::new(SessionSandbox::new(
            &core,
            state,
            enforcing.map(|(landlock, _)| landlock),
            &Level::Autonomous,
            &grant,
            Temp {
                path: workspace.join("tmp"),
                made: true,
            },
            &workspace,
            Limits::default(),
        ));
        let git = SessionGit::new(Vec::new());
        let arguments = vec![OsString::from("status")];
        // Before it is given the sandbox, it starts nothing.
        assert!(
            git.start(Path::new("/usr/bin/git"), arguments.clone())
                .is_err()
        );
        git.set(Arc::clone(&sandbox));
        assert!(git.confines());

        let started = git
            .start(Path::new("/usr/bin/git"), arguments.clone())
            .unwrap();
        assert_eq!(started.program, PathBuf::from("/opt/cortex"));
        assert_eq!(
            started.arguments,
            ["__sandbox", "--", "/usr/bin/git", "status"].map(OsString::from)
        );
        let policy = Policy::decode(&started.stdin.expect("the policy")).unwrap();
        assert_eq!(
            policy,
            Policy {
                read: Reach::All,
                write: Vec::new(),
                network: Network::Off,
                temp: None,
                landlock: 8,
            }
        );

        sandbox.set_level(&Level::Unrestricted);
        assert!(!git.confines());
        let started = git.start(Path::new("/usr/bin/git"), arguments).unwrap();
        assert_eq!(started.program, PathBuf::from("/usr/bin/git"));
        assert_eq!(started.stdin, None);
    }

    /// After a command escapes its kill, the protected paths are looked at
    /// again against the look before that command, so a hook it writes
    /// after its turn is found; only the first escape is remembered.
    #[test]
    fn a_change_after_an_escaped_command_is_found_on_re_check() {
        let workspace = std::env::temp_dir().join(format!(
            "cortex-core-recheck-{:016x}",
            cortex_base::rng::SystemRng::new().next_u64()
        ));
        let hooks = workspace.join(".git/hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        let root =
            cortex_base::AbsPath::from_os(&std::fs::canonicalize(&workspace).unwrap()).unwrap();
        let grant = cortex_policy::default_sandbox(&root, &[], &[]);
        let core = CoreSandbox::new(
            Support::Landlock(8),
            Some((
                PathBuf::from("/opt/cortex"),
                vec![OsString::from("__sandbox")],
            )),
        );
        let (state, enforcing) = core.state(true);
        let sandbox = SessionSandbox::new(
            &core,
            state,
            enforcing.map(|(landlock, _)| landlock),
            &Level::Autonomous,
            &grant,
            Temp {
                path: workspace.join("tmp"),
                made: false,
            },
            root.to_fs_path().as_path(),
            Limits::default(),
        );
        // Nothing escaped yet: no re-check finds anything.
        assert!(sandbox.recheck_escape().is_none());
        // A command runs; the look before it is remembered as if it
        // escaped its kill.
        let before = sandbox.watch().expect("a watch").take();
        sandbox.remember_escape(before);
        // The escaped process writes a hook after the turn.
        std::fs::write(hooks.join("pre-commit"), "#!/bin/sh\ncurl evil\n").unwrap();
        // The re-check finds it, and forgets the escape (a second finds
        // nothing).
        let changed = sandbox.recheck_escape().expect("the hook is found");
        assert!(
            changed
                .iter()
                .any(|one| one.path.ends_with(".git/hooks/pre-commit")),
            "{changed:?}"
        );
        assert!(sandbox.recheck_escape().is_none());
        let _ = std::fs::remove_dir_all(&workspace);
    }
}
