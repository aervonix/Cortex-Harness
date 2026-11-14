//! How the harness starts git, wherever it starts it
//! (`docs/architecture/07-sessions-and-context.md`, "The base state";
//! `docs/architecture/06-permissions-and-sandbox.md`, "The harness's own
//! git"; `docs/security.md`, section 1).
//!
//! Two crates ask git about a workspace: the session store, for the base
//! state and a file's earlier content, and the shell tool, for what a
//! command changed. Neither can see the other, and until step 13 each held
//! its own list of the flags that keep git from writing to the repository
//! or running a program the repository's configuration names. The lists
//! had drifted apart: one turned hooks off and the other pinned the
//! prefixes of a patch. This is the one list, with everything either had,
//! and since the fixes of step 26 the one way to start that git: [`start`].
//!
//! **What no flag turns off** is a program that the repository's
//! configuration names for git to run: a filter for a kind of file, which
//! runs on `git diff` and `git status`; the transport of a fetch that a
//! partial clone makes when an object is missing; and more besides. That
//! is why both callers ask git about a file's content only in a project
//! the user trusts. And trust is not enough once commands are confined: a
//! confined command may write the repository's configuration, which lies
//! in the workspace, after the user trusted it. So [`start`] takes a
//! [`Confinement`], and in a session that confines commands git runs inside
//! the sandbox, reading only, with no network: whatever the configuration
//! makes it start is confined as the command was, and can change nothing
//! (found by the third review of step 26).

#![allow(
    clippy::disallowed_types,
    reason = "starts the harness's own git, which its callers run on the blocking pool"
)]

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

/// What every git command the harness starts begins with, before the
/// subcommand.
///
/// - `--no-optional-locks`: git writes nothing it does not have to, so a
///   look at the repository never refreshes its index behind the user.
/// - `core.fsmonitor=false`: no file-system monitor, which a repository's
///   configuration could name as any program.
/// - `core.hooksPath`: nowhere, so that no hook of the repository runs.
/// - `core.quotepath=off`: names come out as they are, not quoted.
/// - `diff.noprefix=false` and `diff.mnemonicPrefix=false`: a patch has its
///   usual `a/` and `b/` prefixes whatever the user's configuration says,
///   so that it applies.
///
/// A caller also sets [`GIT_ENV`] and closes standard input.
pub const GIT_HARDENING: [&str; 11] = [
    "--no-optional-locks",
    "-c",
    "core.fsmonitor=false",
    "-c",
    if cfg!(windows) {
        "core.hooksPath=NUL"
    } else {
        "core.hooksPath=/dev/null"
    },
    "-c",
    "core.quotepath=off",
    "-c",
    "diff.noprefix=false",
    "-c",
    "diff.mnemonicPrefix=false",
];

/// The environment every git command the harness starts is given: git
/// never asks for a password, takes no optional lock even where an alias
/// or a wrapper drops the flag, and never fetches a missing object of a
/// partial clone on its own (a lazy fetch would start the clone's
/// transport, which the repository's configuration names; the network is
/// off inside the sandbox, but this refuses it outright, before the
/// socket).
pub const GIT_ENV: [(&str, &str); 3] = [
    ("GIT_TERMINAL_PROMPT", "0"),
    ("GIT_OPTIONAL_LOCKS", "0"),
    ("GIT_NO_LAZY_FETCH", "1"),
];

/// The setting that turns git's file-system monitor off, as a name and a
/// value: for a command the harness does not start itself and so cannot
/// give [`GIT_HARDENING`] (a shell command the model wrote), where it goes
/// into the environment as `GIT_CONFIG_KEY_<n>` and `GIT_CONFIG_VALUE_<n>`.
pub const NO_FSMONITOR: (&str, &str) = ("core.fsmonitor", "false");

/// Where the harness's own git runs: as it is, or inside the session's
/// sandbox. The core implements it over the sandbox's launcher; [`AsItIs`]
/// is for a harness whose commands are not confined.
pub trait Confinement: Send + Sync + std::fmt::Debug {
    /// Whether git started now runs inside the sandbox, so that a caller
    /// can say why git failed.
    fn confines(&self) -> bool;

    /// The git program the harness starts: the one found on `PATH`
    /// ([`crate::program::git`]), or, for a session that confines, one
    /// outside its write roots, so that a git a confined command planted
    /// inside a write root is never the one the harness runs (found by the
    /// third review of step 26). `None` when there is no git to run.
    fn git_program(&self) -> Option<PathBuf> {
        crate::program::git().map(Path::to_path_buf)
    }

    /// What to start in place of `git` with `arguments`.
    ///
    /// # Errors
    ///
    /// When git cannot be started as it must be. Then it is not started
    /// at all: an answer git did not give is one the caller does without.
    fn start(&self, git: &Path, arguments: Vec<OsString>) -> Result<Started, String>;
}

/// A program to start in place of git, and what it reads first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Started {
    /// The program: git itself, or the sandbox's launcher.
    pub program: PathBuf,
    /// Its arguments.
    pub arguments: Vec<OsString>,
    /// Bytes for its standard input, which then ends: the launcher reads
    /// its policy there. `None`: a closed input.
    pub stdin: Option<Vec<u8>>,
}

/// Git as it is: where commands are not confined (no sandbox on the
/// system, the `unrestricted` level), so nothing a command wrote got past a
/// boundary that the harness's git would then cross.
#[derive(Debug, Clone, Copy, Default)]
pub struct AsItIs;

impl Confinement for AsItIs {
    fn confines(&self) -> bool {
        false
    }

    fn start(&self, git: &Path, arguments: Vec<OsString>) -> Result<Started, String> {
        Ok(Started {
            program: git.to_owned(),
            arguments,
            stdin: None,
        })
    }
}

/// Why the harness's git was not started.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    /// There is no git in an absolute directory of `PATH`.
    #[error("git is not in an absolute directory of PATH")]
    NotInstalled,
    /// It could not be started as it must be.
    #[error("git could not be started inside the sandbox: {0}")]
    NotConfined(String),
    /// The system did not start it.
    #[error("git could not be started: {0}")]
    Io(#[from] std::io::Error),
}

/// Starts the harness's own git with `arguments`, in `dir`, inside
/// `confinement`: the one way every crate starts it.
///
/// Git is the one found in the absolute directories of `PATH`
/// ([`crate::program::git`]), started by that path, with [`GIT_HARDENING`]
/// before `arguments` and [`GIT_ENV`] in its environment. Its output is a
/// pipe for the caller to read; its standard error is thrown away; its
/// input is closed, after what the confinement gives it to read.
///
/// # Errors
///
/// When there is no git, when the confinement refuses, or when the system
/// cannot start it.
pub fn start(
    confinement: &dyn Confinement,
    dir: &Path,
    arguments: &[&str],
) -> Result<Child, StartError> {
    let git = confinement.git_program().ok_or(StartError::NotInstalled)?;
    let all: Vec<OsString> = GIT_HARDENING
        .iter()
        .chain(arguments)
        .map(OsString::from)
        .collect();
    let started = confinement
        .start(&git, all)
        .map_err(StartError::NotConfined)?;
    let mut command = Command::new(&started.program);
    command
        .args(&started.arguments)
        .current_dir(dir)
        .envs(GIT_ENV)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(if started.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
    let mut child = command.spawn()?;
    if let (Some(bytes), Some(mut input)) = (started.stdin, child.stdin.take()) {
        // A launcher reads its policy to the end before anything else, and
        // a policy is far smaller than a pipe holds. One that is gone
        // already has failed, and its exit says so.
        let _ = input.write_all(&bytes);
    }
    Ok(child)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A confinement that records what git it was asked to start, and
    /// starts git with arguments and an input of its own in its place:
    /// asked to hash what it reads, so that its answer shows the input
    /// arrived.
    #[derive(Debug, Default)]
    struct Recording {
        asked: std::sync::Mutex<Vec<Vec<OsString>>>,
        refuse: bool,
    }

    impl Confinement for Recording {
        fn confines(&self) -> bool {
            true
        }

        fn start(&self, git: &Path, arguments: Vec<OsString>) -> Result<Started, String> {
            if self.refuse {
                return Err("refused".to_owned());
            }
            self.asked.lock().unwrap().push(arguments);
            Ok(Started {
                program: git.to_owned(),
                arguments: ["hash-object", "--stdin"].map(OsString::from).to_vec(),
                stdin: Some(b"hello\n".to_vec()),
            })
        }
    }

    #[test]
    fn sys_git_is_started_as_its_confinement_says() {
        if crate::program::git().is_none() {
            assert!(std::env::var_os("CI").is_none(), "CI has git");
            return;
        }
        let recording = Recording::default();
        let child = start(&recording, &std::env::temp_dir(), &["status", "-z"]).unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        // What the confinement started, with the input it gave.
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            "ce013625030ba8dba906f756967f9e9ca394464a"
        );
        // What it was asked for: the hardening, then the arguments.
        let wanted: Vec<OsString> = GIT_HARDENING
            .iter()
            .chain(&["status", "-z"])
            .map(OsString::from)
            .collect();
        assert_eq!(*recording.asked.lock().unwrap(), [wanted]);

        // One that refuses: nothing starts.
        let refusing = Recording {
            refuse: true,
            ..Recording::default()
        };
        assert!(matches!(
            start(&refusing, &std::env::temp_dir(), &["status"]),
            Err(StartError::NotConfined(_))
        ));
        assert!(!AsItIs.confines());
    }

    #[test]
    fn the_list_is_one_flag_and_then_settings() {
        assert_eq!(GIT_HARDENING[0], "--no-optional-locks");
        let settings: Vec<&str> = GIT_HARDENING[1..]
            .chunks(2)
            .map(|pair| {
                assert_eq!(pair[0], "-c", "every setting is given with -c");
                pair[1]
            })
            .collect();
        for wanted in [
            "core.fsmonitor=false",
            "core.quotepath=off",
            "diff.noprefix=false",
            "diff.mnemonicPrefix=false",
        ] {
            assert!(settings.contains(&wanted), "{wanted}");
        }
        assert!(
            settings
                .iter()
                .any(|setting| setting.starts_with("core.hooksPath=")),
            "hooks are turned off"
        );
        // The monitor is off by the same name in a command's environment.
        assert_eq!(
            format!("{}={}", NO_FSMONITOR.0, NO_FSMONITOR.1),
            "core.fsmonitor=false"
        );
    }
}
