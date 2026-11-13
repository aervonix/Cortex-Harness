//! The environment a command runs with
//! (`docs/architecture/05-tools.md`, "`shell`", "Environment").
//!
//! A command inherits the harness's environment, with three changes.
//!
//! - **Credentials are taken out.** The key variables of the providers the
//!   harness knows ([`DEFAULT_REMOVED`]) are never passed on, whatever the
//!   caller asks for. Configuration adds names to them, and a name may end
//!   with `*` to cover every variable that starts so.
//! - **Tools are told not to ask and not to draw.** No pager, no colour
//!   unless a command asks for it, a terminal that can do nothing, and no
//!   prompt from `git`. A command has no terminal: a tool that waits for a
//!   key would wait until the time limit.
//! - **What the command analysis relies on is made true**
//!   (`docs/architecture/06-permissions-and-sandbox.md`, "Command analysis
//!   as built"). The permission engine reads a command line and decides
//!   from it what the command can touch. Three things in an environment
//!   would make a command do something its line does not say, so they are
//!   settled here ([`ANALYSIS_REMOVED`], and git's file-system monitor):
//!   - `CDPATH` is removed, so that `cd dir` goes where the analysis
//!     followed it and not to a directory of that name somewhere else;
//!   - `RIPGREP_CONFIG_PATH` is removed, so that a configuration file
//!     cannot add `--hidden`, or a preprocessor, to an `rg` the engine
//!     passed as a command that only reads what it names;
//!   - git's file-system monitor is turned off for every git the command
//!     starts. A repository's configuration can name any program as the
//!     monitor, and `git status`, which the engine takes to change
//!     nothing, would start it. The setting is given the way git reads
//!     settings from its environment (`GIT_CONFIG_COUNT`,
//!     `GIT_CONFIG_KEY_<n>`, `GIT_CONFIG_VALUE_<n>`), after whatever the
//!     user's environment already counts, so it wins over theirs and over
//!     the repository's, and theirs stay in force.
//! - **`PATH` keeps only absolute directories.** An empty entry and a
//!   relative one (`.`, `bin`) both mean "look in the directory the
//!   command runs in", which is the workspace: a file the model wrote
//!   there and named `ls` would run in place of the real one, as a command
//!   the engine had passed as `ls`. Such entries are dropped, and the rest
//!   keep their order.

use std::ffi::OsString;

/// The variables that are always removed: the keys of the providers the
/// harness knows. The harness adds the key variable of every provider it
/// is configured with, and what configuration lists.
pub const DEFAULT_REMOVED: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "OPENAI_API_KEY",
    "GEMINI_API_KEY",
    "GOOGLE_API_KEY",
];

/// The variables that are removed because the command analysis takes them
/// to be unset. Not a matter of configuration: a user who could bring one
/// back would be widening what a command the engine allowed can do.
pub const ANALYSIS_REMOVED: &[&str] = &["CDPATH", "RIPGREP_CONFIG_PATH"];

/// How git counts the settings given in its environment.
const GIT_CONFIG_COUNT: &str = "GIT_CONFIG_COUNT";
/// The name of the `n`th setting is in `GIT_CONFIG_KEY_<n>`.
const GIT_CONFIG_KEY: &str = "GIT_CONFIG_KEY_";
/// The value of the `n`th setting is in `GIT_CONFIG_VALUE_<n>`.
const GIT_CONFIG_VALUE: &str = "GIT_CONFIG_VALUE_";

/// What every command is given, whatever the harness's own environment
/// says.
const FIXED: &[(&str, &str)] = &[
    // No pager: `cat` passes everything through, and git reads it as
    // "none".
    ("PAGER", "cat"),
    ("GIT_PAGER", "cat"),
    // No colour, by the convention of no-color.org. A command that passes
    // its own flag for colour still gets it.
    ("NO_COLOR", "1"),
    // A terminal with no abilities: nothing draws progress bars for it.
    ("TERM", "dumb"),
    // Git fails with a message where it would have asked for a password.
    ("GIT_TERMINAL_PROMPT", "0"),
];

/// The environment for a command: `inherited`, less [`DEFAULT_REMOVED`],
/// [`ANALYSIS_REMOVED`] and the variables `removed` names, with the fixed
/// settings in place of whatever was there, and git's file-system monitor
/// turned off after the settings the environment already gives git.
///
/// On Windows a variable's name is matched in any case, as Windows does.
pub(crate) fn build(
    inherited: impl IntoIterator<Item = (OsString, OsString)>,
    removed: &[String],
    windows: bool,
) -> Vec<(OsString, OsString)> {
    let same = |name: &str, pattern: &str| {
        let (pattern, prefix) = match pattern.strip_suffix('*') {
            Some(prefix) => (prefix, true),
            None => (pattern, false),
        };
        let (head, tail) = match name.is_char_boundary(pattern.len()) {
            true if name.len() >= pattern.len() => name.split_at(pattern.len()),
            _ => return false,
        };
        let matches = if windows {
            head.eq_ignore_ascii_case(pattern)
        } else {
            head == pattern
        };
        matches && (prefix || tail.is_empty())
    };
    let mut env: Vec<(OsString, OsString)> = inherited
        .into_iter()
        .filter(|(name, _)| {
            // A name that is not Unicode is no name on either list.
            let Some(name) = name.to_str() else {
                return true;
            };
            !DEFAULT_REMOVED.iter().any(|pattern| same(name, pattern))
                && !ANALYSIS_REMOVED.iter().any(|pattern| same(name, pattern))
                && !removed.iter().any(|pattern| same(name, pattern))
                && !FIXED.iter().any(|(fixed, _)| same(name, fixed))
        })
        .collect();
    env.extend(
        FIXED
            .iter()
            .map(|(name, value)| (OsString::from(name), OsString::from(value))),
    );
    no_fsmonitor(&mut env, &same);
    for (name, value) in &mut env {
        if name.to_str().is_some_and(|name| same(name, PATH)) {
            *value = absolute_entries(value, windows);
        }
    }
    env
}

/// The variable programs are looked up through.
const PATH: &str = "PATH";

/// `path` without its empty and its relative entries, which both name the
/// directory a command runs in.
fn absolute_entries(path: &std::ffi::OsStr, windows: bool) -> OsString {
    let Some(text) = path.to_str() else {
        // Not Unicode: the system's own reading of it, which is what the
        // command would be given anyway.
        let kept = std::env::split_paths(path).filter(|entry| entry.is_absolute());
        return std::env::join_paths(kept).unwrap_or_default();
    };
    let separator = if windows { ';' } else { ':' };
    let absolute = |entry: &str| {
        let bytes = entry.as_bytes();
        if windows {
            // A drive and a root (`C:\`), or a share (`\\server\…`).
            // `C:bin` and `\bin` are relative to where the command is.
            let slash = |byte: &u8| *byte == b'\\' || *byte == b'/';
            (bytes.len() >= 3
                && bytes[0].is_ascii_alphabetic()
                && bytes[1] == b':'
                && slash(&bytes[2]))
                || (bytes.len() >= 2 && slash(&bytes[0]) && slash(&bytes[1]))
        } else {
            bytes.first() == Some(&b'/')
        }
    };
    let kept: Vec<&str> = text
        .split(separator)
        // Windows allows an entry in quotes, for one with a semicolon.
        .filter(|entry| absolute(entry.trim_matches('"')))
        .collect();
    OsString::from(kept.join(&separator.to_string()))
}

/// Adds `core.fsmonitor=false` to the settings git reads from its
/// environment, as the last of them: git takes the last value of a setting
/// it is given twice, and settings from the environment win over every
/// configuration file.
fn no_fsmonitor(env: &mut Vec<(OsString, OsString)>, same: &dyn Fn(&str, &str) -> bool) {
    let is = |name: &OsString, wanted: &str| name.to_str().is_some_and(|name| same(name, wanted));
    // What the user's environment counts. A count git itself would refuse
    // (not a number) counts nothing: the command then runs with ours alone,
    // where it would have failed.
    let count = env
        .iter()
        .find(|(name, _)| is(name, GIT_CONFIG_COUNT))
        .and_then(|(_, value)| value.to_str()?.trim().parse::<u32>().ok())
        .unwrap_or(0);
    let key = format!("{GIT_CONFIG_KEY}{count}");
    let value = format!("{GIT_CONFIG_VALUE}{count}");
    // A leftover pair at our number, beyond the count, is not a setting
    // git reads; it must not be passed beside ours under the same name.
    env.retain(|(name, _)| !is(name, GIT_CONFIG_COUNT) && !is(name, &key) && !is(name, &value));
    let (setting, off) = cortex_base::git::NO_FSMONITOR;
    env.push((
        OsString::from(GIT_CONFIG_COUNT),
        OsString::from(count.saturating_add(1).to_string()),
    ));
    env.push((OsString::from(key), OsString::from(setting)));
    env.push((OsString::from(value), OsString::from(off)));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        pairs
            .iter()
            .map(|(name, value)| (OsString::from(name), OsString::from(value)))
            .collect()
    }

    fn get<'a>(env: &'a [(OsString, OsString)], name: &str) -> Vec<&'a str> {
        env.iter()
            .filter(|(key, _)| key == name)
            .map(|(_, value)| value.to_str().expect("text"))
            .collect()
    }

    /// Nothing asked for: the provider keys go all the same.
    fn defaults() -> Vec<String> {
        Vec::new()
    }

    #[test]
    fn credentials_are_removed_and_the_rest_is_inherited() {
        let built = build(
            env(&[
                ("PATH", "/usr/bin"),
                ("LANG", "C.UTF-8"),
                ("ANTHROPIC_API_KEY", "sk-test-key"),
                ("OPENAI_API_KEY", "sk-other"),
                ("ANTHROPIC_API_KEY_BACKUP", "kept: another name"),
                ("MY_TOKEN", "kept: not on the list"),
            ]),
            &defaults(),
            false,
        );
        assert_eq!(get(&built, "PATH"), ["/usr/bin"]);
        assert_eq!(get(&built, "LANG"), ["C.UTF-8"]);
        assert!(get(&built, "ANTHROPIC_API_KEY").is_empty());
        assert!(get(&built, "OPENAI_API_KEY").is_empty());
        assert_eq!(
            get(&built, "ANTHROPIC_API_KEY_BACKUP"),
            ["kept: another name"]
        );
        assert_eq!(get(&built, "MY_TOKEN"), ["kept: not on the list"]);
        // Nowhere, under any name.
        assert!(
            !built
                .iter()
                .any(|(_, value)| value.to_str().is_some_and(|value| value.contains("sk-")))
        );
    }

    #[test]
    fn a_name_ending_with_a_star_covers_everything_that_starts_so() {
        let removed = vec!["SECRET_*".to_owned(), "EXACT".to_owned()];
        let built = build(
            env(&[
                ("SECRET_ONE", "1"),
                ("SECRET_", "2"),
                ("SECRETS", "kept"),
                ("EXACT", "3"),
                ("EXACTLY", "kept"),
            ]),
            &removed,
            false,
        );
        assert!(get(&built, "SECRET_ONE").is_empty());
        assert!(get(&built, "SECRET_").is_empty());
        assert!(get(&built, "EXACT").is_empty());
        assert_eq!(get(&built, "SECRETS"), ["kept"]);
        assert_eq!(get(&built, "EXACTLY"), ["kept"]);
    }

    #[test]
    fn windows_matches_names_in_any_case_and_linux_does_not() {
        let inherited = [
            ("Anthropic_Api_Key", "sk-test-key"),
            ("Path", "C:\\Windows"),
        ];
        let on_windows = build(env(&inherited), &defaults(), true);
        assert!(get(&on_windows, "Anthropic_Api_Key").is_empty());
        assert_eq!(get(&on_windows, "Path"), ["C:\\Windows"]);
        // On Linux that is another variable.
        let on_linux = build(env(&inherited), &defaults(), false);
        assert_eq!(get(&on_linux, "Anthropic_Api_Key"), ["sk-test-key"]);
    }

    #[test]
    fn what_the_command_analysis_relies_on_is_removed() {
        let built = build(
            env(&[
                ("CDPATH", "/somewhere/else"),
                ("RIPGREP_CONFIG_PATH", "/etc/ripgreprc"),
                ("CDPATHS", "kept: another name"),
                ("PATH", "/usr/bin"),
            ]),
            &defaults(),
            false,
        );
        assert!(get(&built, "CDPATH").is_empty());
        assert!(get(&built, "RIPGREP_CONFIG_PATH").is_empty());
        assert_eq!(get(&built, "CDPATHS"), ["kept: another name"]);
        assert_eq!(get(&built, "PATH"), ["/usr/bin"]);
        // In any case on Windows, where a name has none.
        let on_windows = build(
            env(&[("CdPath", "C:\\else"), ("Ripgrep_Config_Path", "C:\\rc")]),
            &defaults(),
            true,
        );
        assert!(get(&on_windows, "CdPath").is_empty());
        assert!(get(&on_windows, "Ripgrep_Config_Path").is_empty());
    }

    #[test]
    fn path_keeps_only_its_absolute_directories() {
        let built = build(
            env(&[(
                "PATH",
                ".:/usr/local/bin::/usr/bin:bin:./tools:../up:/opt/my tools/bin:",
            )]),
            &defaults(),
            false,
        );
        assert_eq!(
            get(&built, "PATH"),
            ["/usr/local/bin:/usr/bin:/opt/my tools/bin"]
        );
        // Nothing but entries that name where the command runs: nothing is
        // left, and the variable stays, empty.
        let built = build(env(&[("PATH", ".::bin")]), &defaults(), false);
        assert_eq!(get(&built, "PATH"), [""]);
        // Another variable that holds a list is left alone.
        let built = build(env(&[("MANPATH", ".:/usr/share/man")]), &defaults(), false);
        assert_eq!(get(&built, "MANPATH"), [".:/usr/share/man"]);
    }

    #[test]
    fn on_windows_path_is_found_under_any_spelling_and_read_as_windows_reads_it() {
        let built = build(
            env(&[(
                "Path",
                r#".;C:\Windows\System32;;bin;C:tools;\rooted;"C:\Program Files\Git\cmd";\\server\share\bin;D:/msys/bin"#,
            )]),
            &defaults(),
            true,
        );
        assert_eq!(
            get(&built, "Path"),
            [r#"C:\Windows\System32;"C:\Program Files\Git\cmd";\\server\share\bin;D:/msys/bin"#]
        );
    }

    #[test]
    fn gits_file_system_monitor_is_turned_off() {
        let built = build(env(&[("PATH", "/usr/bin")]), &defaults(), false);
        assert_eq!(get(&built, "GIT_CONFIG_COUNT"), ["1"]);
        assert_eq!(get(&built, "GIT_CONFIG_KEY_0"), ["core.fsmonitor"]);
        assert_eq!(get(&built, "GIT_CONFIG_VALUE_0"), ["false"]);
    }

    #[test]
    fn the_monitor_setting_comes_after_the_settings_the_user_already_gives_git() {
        let built = build(
            env(&[
                ("GIT_CONFIG_COUNT", "2"),
                ("GIT_CONFIG_KEY_0", "user.name"),
                ("GIT_CONFIG_VALUE_0", "Dev"),
                ("GIT_CONFIG_KEY_1", "core.fsmonitor"),
                ("GIT_CONFIG_VALUE_1", "/usr/local/bin/watcher"),
                // Beyond the count: git does not read these, and they must
                // not stand beside ours.
                ("GIT_CONFIG_KEY_2", "core.pager"),
                ("GIT_CONFIG_VALUE_2", "less"),
            ]),
            &defaults(),
            false,
        );
        assert_eq!(get(&built, "GIT_CONFIG_COUNT"), ["3"]);
        // The user's own stay as they were.
        assert_eq!(get(&built, "GIT_CONFIG_KEY_0"), ["user.name"]);
        assert_eq!(get(&built, "GIT_CONFIG_VALUE_0"), ["Dev"]);
        assert_eq!(get(&built, "GIT_CONFIG_KEY_1"), ["core.fsmonitor"]);
        assert_eq!(
            get(&built, "GIT_CONFIG_VALUE_1"),
            ["/usr/local/bin/watcher"]
        );
        // Ours is the last, so it is the one git takes.
        assert_eq!(get(&built, "GIT_CONFIG_KEY_2"), ["core.fsmonitor"]);
        assert_eq!(get(&built, "GIT_CONFIG_VALUE_2"), ["false"]);
    }

    #[test]
    fn a_count_git_would_refuse_counts_nothing() {
        for count in ["", "many", "-1", "1.5"] {
            let built = build(
                env(&[("GIT_CONFIG_COUNT", count), ("GIT_CONFIG_KEY_0", "old")]),
                &defaults(),
                false,
            );
            assert_eq!(get(&built, "GIT_CONFIG_COUNT"), ["1"], "{count:?}");
            assert_eq!(get(&built, "GIT_CONFIG_KEY_0"), ["core.fsmonitor"]);
            assert_eq!(get(&built, "GIT_CONFIG_VALUE_0"), ["false"]);
        }
        // Spaces around a number are read as git reads them.
        let built = build(env(&[("GIT_CONFIG_COUNT", " 4 ")]), &defaults(), false);
        assert_eq!(get(&built, "GIT_CONFIG_COUNT"), ["5"]);
        assert_eq!(get(&built, "GIT_CONFIG_KEY_4"), ["core.fsmonitor"]);
    }

    #[test]
    fn on_windows_the_count_is_found_under_any_spelling() {
        let built = build(
            env(&[
                ("Git_Config_Count", "1"),
                ("GIT_CONFIG_KEY_0", "user.name"),
                ("GIT_CONFIG_VALUE_0", "Dev"),
            ]),
            &defaults(),
            true,
        );
        assert!(get(&built, "Git_Config_Count").is_empty());
        assert_eq!(get(&built, "GIT_CONFIG_COUNT"), ["2"]);
        assert_eq!(get(&built, "GIT_CONFIG_KEY_1"), ["core.fsmonitor"]);
        assert_eq!(get(&built, "GIT_CONFIG_VALUE_1"), ["false"]);
    }

    #[test]
    fn the_fixed_settings_replace_what_was_inherited() {
        let built = build(
            env(&[
                ("TERM", "xterm-256color"),
                ("PAGER", "less"),
                ("NO_COLOR", ""),
                ("term", "lower"),
            ]),
            &[],
            false,
        );
        assert_eq!(get(&built, "TERM"), ["dumb"]);
        assert_eq!(get(&built, "PAGER"), ["cat"]);
        assert_eq!(get(&built, "GIT_PAGER"), ["cat"]);
        assert_eq!(get(&built, "NO_COLOR"), ["1"]);
        assert_eq!(get(&built, "GIT_TERMINAL_PROMPT"), ["0"]);
        // Another variable on Linux, and the same one on Windows, where
        // two spellings of one name must not both be passed.
        assert_eq!(get(&built, "term"), ["lower"]);
        let on_windows = build(env(&[("term", "lower"), ("Pager", "more")]), &[], true);
        assert!(get(&on_windows, "term").is_empty());
        assert!(get(&on_windows, "Pager").is_empty());
        assert_eq!(get(&on_windows, "TERM"), ["dumb"]);
        assert_eq!(get(&on_windows, "PAGER"), ["cat"]);
    }
}
