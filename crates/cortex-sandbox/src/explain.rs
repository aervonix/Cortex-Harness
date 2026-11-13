//! What the sandbox did to a command that ended badly, in one line for the
//! model (`docs/architecture/06-permissions-and-sandbox.md`, "Leaving the
//! sandbox").
//!
//! The kernel does not tell a process's parent which rule refused what, so
//! the line is read from what the command printed:
//!
//! - **The launcher's own line** ([`MARK`](crate::launch::MARK)): the
//!   command did not run, and why.
//! - **A write outside the write roots:** a line that reports a refusal
//!   (`Permission denied`, `Operation not permitted`, `EACCES`, `EPERM`,
//!   `Read-only file system`) and names an absolute path that no write
//!   root holds.
//! - **The network,** when it is off: the messages programs print when
//!   they cannot make a socket or look up a name.
//!
//! It names only what it can name. A refusal of a path inside the
//! workspace is the file system's own (a file's mode), not the sandbox's,
//! and is not explained as one.

use std::path::{Path, PathBuf};

use crate::launch::{FAILED, MARK};
use crate::policy::{Network, Policy};

/// What a line says when the operating system refused something.
const REFUSALS: &[&str] = &[
    "permission denied",
    "operation not permitted",
    "eacces",
    "eperm",
    "read-only file system",
    "errno 13",
    "errno 1]",
    "os error 13",
    "os error 1)",
];

/// What programs print when they cannot reach the network.
const NETWORK: &[&str] = &[
    "could not resolve host",
    "couldn't resolve host",
    "temporary failure in name resolution",
    "name or service not known",
    "failed to resolve",
    "getaddrinfo",
    "eai_again",
    "enotfound",
    "network is unreachable",
    "failed to establish a new connection",
    "socket: operation not permitted",
    "could not create socket",
    "couldn't connect to server",
    "failed to connect to",
    "dial tcp",
    "connection refused",
    "no route to host",
    "unable to access 'http",
];

/// The way out the model is told of.
const LEAVE: &str = "To run it outside the sandbox, call `shell` again with `\"sandbox\": false`; \
                     the user is asked first.";

/// The line for a command that ended with `code` after printing `output`.
#[must_use]
pub fn explain(policy: &Policy, code: Option<i32>, output: &str) -> Option<String> {
    if code == Some(0) {
        return None;
    }
    let lines = || output.lines().map(str::trim);
    if let Some(line) = lines().find(|line| line.starts_with(MARK))
        && matches!(code, Some(FAILED | 126 | 127))
    {
        let why = line[MARK.len()..].trim();
        return Some(format!("[The sandbox launcher stopped: {why}]"));
    }
    for line in lines() {
        let lower = line.to_lowercase();
        if !REFUSALS.iter().any(|refusal| lower.contains(refusal)) {
            continue;
        }
        // The path a refusal is about comes last on its line, after the
        // program's own name (`/bin/sh: 1: cannot create /x: Permission
        // denied`) and the source of a move.
        let paths: Vec<PathBuf> = paths_in(line).collect();
        if let Some(path) = paths.into_iter().rev().find(|path| outside(policy, path)) {
            return Some(format!(
                "[The sandbox refused a write to {}: commands may write only under {}. {LEAVE}]",
                path.display(),
                roots(policy)
            ));
        }
    }
    if policy.network == Network::Off
        && lines().any(|line| {
            let lower = line.to_lowercase();
            NETWORK.iter().any(|sign| lower.contains(sign))
        })
    {
        return Some(format!(
            "[The sandbox keeps commands off the network (`sandbox.network = \"off\"`): no \
             connection can be made, to this machine's own addresses included. {LEAVE}]"
        ));
    }
    None
}

/// Whether the sandbox, and not the file system, would refuse to write
/// `path`.
fn outside(policy: &Policy, path: &Path) -> bool {
    path != Path::new("/dev/null") && !policy.may_write(path)
}

/// The write roots, for a sentence.
fn roots(policy: &Policy) -> String {
    match policy.write.as_slice() {
        [] => "nothing: this command may write nowhere".to_owned(),
        roots => roots
            .iter()
            .map(|root| root.display().to_string())
            .collect::<Vec<_>>()
            .join(", "),
    }
}

/// The absolute paths a line names, as programs quote them: bare, or in
/// single, double or typographic quotes, with a trailing `:` or `,` left
/// off.
fn paths_in(line: &str) -> impl Iterator<Item = PathBuf> + '_ {
    line.split(|c: char| c.is_whitespace() || matches!(c, '\'' | '"' | '`' | '‘' | '’' | '(' | ')'))
        .map(|word| word.trim_end_matches([':', ',', ';', '.']))
        .filter(|word| word.starts_with('/') && word.len() > 1)
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::Reach;

    fn policy(network: Network) -> Policy {
        Policy {
            read: Reach::All,
            write: vec![PathBuf::from("/work"), PathBuf::from("/tmp/cx-s1")],
            network,
            temp: Some(PathBuf::from("/tmp/cx-s1")),
            landlock: 6,
        }
    }

    #[test]
    fn a_refused_write_outside_the_roots_is_named() {
        let policy = policy(Network::Off);
        for (output, path) in [
            (
                "touch: cannot touch '/srv/u/x': Permission denied",
                "/srv/u/x",
            ),
            ("bash: /etc/hosts: Permission denied", "/etc/hosts"),
            (
                "/bin/sh: 1: cannot create /srv/u/new: Permission denied",
                "/srv/u/new",
            ),
            (
                "mv: cannot move '/work/a' to '/srv/u/a': Permission denied",
                "/srv/u/a",
            ),
            (
                "PermissionError: [Errno 13] Permission denied: '/srv/u/.cache/pip'",
                "/srv/u/.cache/pip",
            ),
            (
                "Error: EACCES: permission denied, open '/srv/u/.npm/_cacache/tmp'",
                "/srv/u/.npm/_cacache/tmp",
            ),
            (
                "mkdir: cannot create directory ‘/opt/y’: Permission denied",
                "/opt/y",
            ),
        ] {
            let line = explain(&policy, Some(1), output).expect(output);
            assert!(line.contains(path), "{line}");
            assert!(line.contains("/work, /tmp/cx-s1"), "{line}");
            assert!(line.contains("\"sandbox\": false"), "{line}");
        }
    }

    #[test]
    fn a_refusal_inside_the_roots_is_not_the_sandboxs() {
        let policy = policy(Network::Full);
        for output in [
            "touch: cannot touch '/work/locked': Permission denied",
            "bash: /dev/null: Permission denied",
            "rm: cannot remove 'relative': Permission denied",
            "Permission denied (publickey).",
        ] {
            assert_eq!(explain(&policy, Some(1), output), None, "{output}");
        }
        // A command that succeeded is left alone, whatever it printed.
        assert_eq!(
            explain(
                &policy,
                Some(0),
                "touch: cannot touch '/etc/x': Permission denied"
            ),
            None
        );
    }

    #[test]
    fn a_network_failure_is_named_only_when_the_network_is_off() {
        for output in [
            "curl: (6) Could not resolve host: example.com",
            "fatal: unable to access 'https://github.com/a/b/': Could not resolve host: github.com",
            "socket: Operation not permitted",
            "urllib.error.URLError: <urlopen error [Errno -3] Temporary failure in name resolution>",
            "npm error getaddrinfo EAI_AGAIN registry.npmjs.org",
        ] {
            let line = explain(&policy(Network::Off), Some(6), output).expect(output);
            assert!(line.contains("network"), "{line}");
            assert_eq!(explain(&policy(Network::Full), Some(6), output), None);
        }
    }

    #[test]
    fn the_launchers_own_line_says_why_the_command_did_not_run() {
        let output = format!(
            "{MARK} the sandbox could not be applied, so the command was not run: seccomp: x"
        );
        let line = explain(&policy(Network::Off), Some(FAILED), &output).unwrap();
        assert!(
            line.contains("seccomp: x") && line.contains("not run"),
            "{line}"
        );
        // The same text from a command that exited otherwise is not the
        // launcher's.
        assert_eq!(explain(&policy(Network::Full), Some(3), &output), None);
    }
}
