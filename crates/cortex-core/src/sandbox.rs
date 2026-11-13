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

#![allow(
    clippy::disallowed_methods,
    reason = "the temporary directory is made while a session is assembled and removed when it \
              ends, both on the blocking pool"
)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use cortex_policy::{NoSandbox, SandboxState};
use cortex_protocol::SessionId;
use cortex_protocol::lease::{Level, NetworkAccess, SandboxPolicy};
use cortex_protocol::session::SandboxStatus;
use cortex_sandbox::launch::Launcher as SandboxLauncher;
use cortex_sandbox::policy::{Network, Policy, Reach};
use cortex_sandbox::protect::{Changed, Limits, Watch};
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
    /// Whether commands are confined now: the sandbox enforces and the
    /// level is not `unrestricted`.
    confine: AtomicBool,
    /// The private temporary directory.
    temp: Temp,
    /// The protected paths, looked at before and after each confined
    /// command, when the sandbox enforces.
    watch: Option<Watch>,
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
        let launcher = match (state, landlock, &core.program) {
            (SandboxState::Enforcing, Some(landlock), Some((program, prefix))) => {
                Some(SandboxLauncher::new(
                    program.clone(),
                    prefix.clone(),
                    policy_of(grant, &temp, landlock),
                ))
            }
            _ => None,
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
            confine: AtomicBool::new(false),
            temp,
            watch,
        };
        sandbox.set_level(level);
        sandbox
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
fn policy_of(grant: &SandboxPolicy, temp: &Temp, landlock: u8) -> Policy {
    let roots = |roots: &[String]| roots.iter().map(PathBuf::from).collect::<Vec<_>>();
    let mut write = roots(&grant.write.roots);
    // A temporary directory that was not made, or is not the session's own,
    // is not written to: someone else could read it, or it leads elsewhere.
    if !temp.made {
        write.retain(|root| *root != temp.path);
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
        temp: temp.made.then(|| temp.path.clone()),
        landlock,
    }
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
