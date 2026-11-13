//! The launcher: the harness's own executable, under a hidden subcommand,
//! which applies the policy to itself and then becomes the command
//! (`docs/architecture/06-permissions-and-sandbox.md`, "How it is applied").
//!
//! - [`Launcher`] is the harness's side: what to start in place of a
//!   command, and the bytes of the policy for that process's standard
//!   input.
//! - [`run`] is the launcher's side, the body of the hidden subcommand. It
//!   reads the policy to its end, confines itself, gives the command a
//!   closed input and the session's temporary directory, and replaces
//!   itself with the command.
//!
//! **It fails closed.** Once the harness has said the sandbox enforces, a
//! policy that cannot be read, a Landlock rule the kernel refuses, a
//! seccomp filter that does not load, or a build without the backends
//! stops the launcher before the command runs: it writes one line that
//! begins with [`MARK`] and exits with [`FAILED`]. Nothing is run
//! unconfined in its place.
//!
//! The launcher is a fresh, single-threaded process, so nothing runs
//! between `fork` and `exec` of a multi-threaded harness. The process tree
//! (its session, its group, the death signal, the guard) is set up by the
//! runner before the launcher starts, so the launcher does not touch it.

use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::explain;
use crate::policy::{MAX_BYTES, Policy};

/// What every line the launcher writes about itself begins with.
pub const MARK: &str = "cortex-sandbox:";

/// The exit code of a launcher that did not start the command because the
/// sandbox could not be applied. The same code `env` and `timeout` use for
/// a failure of their own.
pub const FAILED: i32 = 125;

/// The harness's side of the launcher: one per session, holding the
/// session's policy.
#[derive(Debug, Clone)]
pub struct Launcher {
    program: PathBuf,
    prefix: Vec<OsString>,
    policy: Policy,
    encoded: Vec<u8>,
}

/// What to start in place of a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wrapped {
    /// The program: the harness's own executable.
    pub program: PathBuf,
    /// Its arguments: the hidden subcommand, `--`, and the command.
    pub arguments: Vec<OsString>,
    /// The policy, for the program's standard input.
    pub stdin: Vec<u8>,
}

impl Launcher {
    /// A launcher that starts `program` with `prefix` (the harness's own
    /// executable and its hidden subcommand, whose body is [`run`]) to
    /// apply `policy`.
    #[must_use]
    pub fn new(
        program: impl Into<PathBuf>,
        prefix: impl IntoIterator<Item = impl Into<OsString>>,
        policy: Policy,
    ) -> Self {
        let encoded = policy.encode();
        Self {
            program: program.into(),
            prefix: prefix.into_iter().map(Into::into).collect(),
            policy,
            encoded,
        }
    }

    /// The policy it applies.
    #[must_use]
    pub fn policy(&self) -> &Policy {
        &self.policy
    }

    /// What to start in place of `program` with `arguments`.
    #[must_use]
    pub fn wrap(&self, program: &OsStr, arguments: &[OsString]) -> Wrapped {
        let mut all = self.prefix.clone();
        all.push(OsString::from("--"));
        all.push(program.to_owned());
        all.extend(arguments.iter().cloned());
        Wrapped {
            program: self.program.clone(),
            arguments: all,
            stdin: self.encoded.clone(),
        }
    }

    /// One line for the model about what the sandbox did to a command that
    /// has ended, read from its exit code and its output: which operation
    /// it blocked, or why the command did not run. `None` when there is
    /// nothing to say.
    #[must_use]
    pub fn explain(&self, code: Option<i32>, output: &str) -> Option<String> {
        explain::explain(&self.policy, code, output)
    }
}

/// The body of the hidden subcommand: `arguments` are what follows it,
/// `--` and then the command.
///
/// It returns only when the command could not be started, with the exit
/// code to end with, after writing why on its standard error, which is the
/// command's output.
#[must_use]
pub fn run(arguments: Vec<OsString>) -> i32 {
    match launch(arguments) {
        Ok(never) => match never {},
        Err(Failure { code, message }) => {
            // The command's output is this process's standard error: the
            // line reaches the result.
            #[expect(clippy::print_stderr, reason = "the launcher's one line is its output")]
            {
                eprintln!("{MARK} {message}");
            }
            code
        }
    }
}

/// Why the launcher did not become the command.
struct Failure {
    code: i32,
    message: String,
}

impl Failure {
    fn sandbox(message: impl std::fmt::Display) -> Self {
        Self {
            code: FAILED,
            message: format!(
                "the sandbox could not be applied, so the command was not run: {message}"
            ),
        }
    }
}

/// The uninhabited type of a launch that succeeded: the process is the
/// command now.
enum Never {}

fn launch(arguments: Vec<OsString>) -> Result<Never, Failure> {
    let mut arguments = arguments.into_iter();
    if arguments.next().as_deref() != Some(OsStr::new("--")) {
        return Err(Failure::sandbox("the launcher was not given a command"));
    }
    let Some(program) = arguments.next() else {
        return Err(Failure::sandbox("the launcher was not given a command"));
    };
    let rest: Vec<OsString> = arguments.collect();

    let mut bytes = Vec::new();
    std::io::stdin()
        .lock()
        .take(u64::try_from(MAX_BYTES).unwrap_or(u64::MAX) + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| Failure::sandbox(format!("the policy could not be read: {error}")))?;
    let policy = Policy::decode(&bytes).map_err(Failure::sandbox)?;

    confine(&policy).map_err(Failure::sandbox)?;
    become_command(&program, &rest, policy.temp.as_deref())
}

/// Applies the policy to this process and everything it will start.
#[cfg(all(target_os = "linux", feature = "backends"))]
fn confine(policy: &Policy) -> Result<(), String> {
    crate::confine::apply(policy)
}

#[cfg(not(all(target_os = "linux", feature = "backends")))]
fn confine(_policy: &Policy) -> Result<(), String> {
    Err("this build of Cortex-Harness has no sandbox".to_owned())
}

/// Replaces this process with the command: a closed input, the session's
/// temporary directory, and nothing of the launcher's left open.
#[cfg(unix)]
#[allow(
    clippy::disallowed_types,
    reason = "the launcher is a process of its own, with one thread and no runtime, and this is \
              where it becomes the command"
)]
fn become_command(
    program: &OsStr,
    arguments: &[OsString],
    temp: Option<&Path>,
) -> Result<Never, Failure> {
    use std::os::unix::process::CommandExt;

    close_on_exec();
    let mut command = std::process::Command::new(program);
    command.args(arguments).stdin(std::process::Stdio::null());
    if let Some(temp) = temp {
        command.env("TMPDIR", temp);
    }
    let error = command.exec();
    let code = match error.kind() {
        std::io::ErrorKind::NotFound => 127,
        _ => 126,
    };
    Err(Failure {
        code,
        message: format!(
            "`{}` could not be started inside the sandbox: {error}",
            Path::new(program).display()
        ),
    })
}

#[cfg(not(unix))]
fn become_command(
    _program: &OsStr,
    _arguments: &[OsString],
    _temp: Option<&Path>,
) -> Result<Never, Failure> {
    Err(Failure::sandbox(
        "there is no sandbox launcher on this system",
    ))
}

/// Marks every descriptor above the standard three to be closed when the
/// command starts, so that nothing the launcher inherited by mistake (a
/// socket above all) reaches the command.
#[cfg(unix)]
#[allow(
    unsafe_code,
    reason = "decision 0030: marking descriptors is only reachable through a system call; \
              cortex-sandbox is where such calls live"
)]
fn close_on_exec() {
    #[cfg(target_os = "linux")]
    {
        /// `CLOSE_RANGE_CLOEXEC`: mark, do not close.
        const CLOEXEC: libc::c_uint = 1 << 2;
        // SAFETY: the call takes three integers and touches no memory of
        // this process. It marks descriptors; it closes none, so no handle
        // the standard library holds is invalidated.
        let _ = unsafe { libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, CLOEXEC) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{Network, Reach};

    #[test]
    fn a_wrapped_command_keeps_every_argument_after_the_marker() {
        let policy = Policy {
            read: Reach::All,
            write: Vec::new(),
            network: Network::Off,
            temp: None,
            landlock: 6,
        };
        let launcher = Launcher::new("/opt/cortex", ["__sandbox"], policy.clone());
        let wrapped = launcher.wrap(
            OsStr::new("bash"),
            &[OsString::from("-c"), OsString::from("echo -- 'x y'")],
        );
        assert_eq!(wrapped.program, PathBuf::from("/opt/cortex"));
        assert_eq!(
            wrapped.arguments,
            ["__sandbox", "--", "bash", "-c", "echo -- 'x y'"]
                .map(OsString::from)
                .to_vec()
        );
        assert_eq!(Policy::decode(&wrapped.stdin).unwrap(), policy);
    }
}
