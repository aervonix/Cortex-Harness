//! The environment a command runs with
//! (`docs/architecture/05-tools.md`, "`shell`", "Environment").
//!
//! A command inherits the harness's environment, with two changes.
//!
//! - **Credentials are taken out.** The key variables of the providers the
//!   harness knows ([`DEFAULT_REMOVED`]) are never passed on, whatever the
//!   caller asks for. Configuration adds names to them, and a name may end
//!   with `*` to cover every variable that starts so.
//! - **Tools are told not to ask and not to draw.** No pager, no colour
//!   unless a command asks for it, a terminal that can do nothing, and no
//!   prompt from `git`. A command has no terminal: a tool that waits for a
//!   key would wait until the time limit.

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

/// The environment for a command: `inherited`, less [`DEFAULT_REMOVED`]
/// and the variables `removed` names, with the fixed settings in place of
/// whatever was there.
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
                && !removed.iter().any(|pattern| same(name, pattern))
                && !FIXED.iter().any(|(fixed, _)| same(name, fixed))
        })
        .collect();
    env.extend(
        FIXED
            .iter()
            .map(|(name, value)| (OsString::from(name), OsString::from(value))),
    );
    env
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
