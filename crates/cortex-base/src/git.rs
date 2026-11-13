//! How the harness starts git, wherever it starts it
//! (`docs/architecture/07-sessions-and-context.md`, "The base state";
//! `docs/security.md`, section 1).
//!
//! Two crates ask git about a workspace: the session store, for the base
//! state, and the shell tool, for what a command changed. Neither can see
//! the other, and until step 13 each held its own list of the flags that
//! keep git from writing to the repository or running a program the
//! repository's configuration names. The lists had drifted apart: one
//! turned hooks off and the other pinned the prefixes of a patch. This is
//! the one list, with everything either had.
//!
//! **What no flag turns off** is a filter that the repository's
//! configuration names for a kind of file, which runs on `git diff` and
//! `git status`. That is why both callers ask git about a file's content
//! only in a project the user trusts.

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
/// never asks for a password, and takes no optional lock even where an
/// alias or a wrapper drops the flag.
pub const GIT_ENV: [(&str, &str); 2] = [("GIT_TERMINAL_PROMPT", "0"), ("GIT_OPTIONAL_LOCKS", "0")];

/// The setting that turns git's file-system monitor off, as a name and a
/// value: for a command the harness does not start itself and so cannot
/// give [`GIT_HARDENING`] (a shell command the model wrote), where it goes
/// into the environment as `GIT_CONFIG_KEY_<n>` and `GIT_CONFIG_VALUE_<n>`.
pub const NO_FSMONITOR: (&str, &str) = ("core.fsmonitor", "false");

#[cfg(test)]
mod tests {
    use super::*;

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
