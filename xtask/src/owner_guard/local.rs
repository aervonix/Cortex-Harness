//! The owner guard before a push: `cargo xtask owner-guard --pre-push`,
//! which the git hook `xtask/hooks/pre-push` runs, and `--unpushed`, the
//! same check by hand (`docs/testing-and-ci.md`, "The owner guard").
//!
//! CI's guard stops a merge, not a leak: a branch is public as soon as it
//! is pushed, and a commit stays reachable by its hash after it is
//! rewritten. This one runs before the push leaves the desk.
//!
//! - **The terms** are in a file outside the repository, which the owner
//!   keeps: `~/.config/cortex-harness/owner-terms` on every OS (the home
//!   directory is `HOME`, or `USERPROFILE` on Windows), or the file that
//!   `CORTEX_OWNER_TERMS_FILE` names. They are matched as CI matches
//!   `OWNER_TERMS`.
//! - **What a push adds** is every commit it sends that the remote's
//!   branches, as this repository last saw them, do not have: each one's
//!   own added lines and paths (a term a later commit takes out is still in
//!   the earlier one, which stays reachable), its message, author and
//!   committer; the names of the refs it writes; and a tag's own text.
//! - **A term is never printed,** and neither is any path that holds the
//!   home directory: a finding names the commit and the field, or the file
//!   and the line, and every line this module prints is first held against
//!   the terms.
//! - **Without the file it passes, with a warning,** as CI does without
//!   the secret, so that a contributor who does not have it is not
//!   blocked. A file that `CORTEX_OWNER_TERMS_FILE` names and that is not
//!   there is a refusal: someone asked for it. **When the file is there and
//!   the check cannot run** (the file cannot be read, holds no term, is
//!   inside the repository, or git fails), the push is refused, with the
//!   reason.

use std::collections::HashSet;
use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};

use super::{Finding, LOG_FORMAT, Terms, git, scan_commits, scan_diff, scan_paths};
use crate::Result;

/// The variable that names a terms file in place of the default.
pub(crate) const TERMS_FILE: &str = "CORTEX_OWNER_TERMS_FILE";

/// The default file, under the home directory, as it is printed.
const DEFAULT_SHOWN: &str = "~/.config/cortex-harness/owner-terms";

/// What git writes for an object that is not there: a new branch's remote
/// side, a deleted branch's local side.
fn is_zero(object: &str) -> bool {
    object.bytes().all(|byte| byte == b'0')
}

/// Where the terms are looked for.
#[derive(Debug, PartialEq)]
pub(crate) enum TermsFile {
    /// The file `CORTEX_OWNER_TERMS_FILE` names: it must be there.
    Named(PathBuf),
    /// The default file: when it is not there, nothing is checked.
    Default(PathBuf),
    /// No home directory to look in.
    Nowhere,
}

impl TermsFile {
    /// Where to look, from the environment `var` reads.
    pub(crate) fn find(var: impl Fn(&str) -> Option<OsString>) -> Self {
        if let Some(named) = var(TERMS_FILE).filter(|value| !value.is_empty()) {
            return Self::Named(PathBuf::from(named));
        }
        let home = var("HOME")
            .filter(|value| !value.is_empty())
            .or_else(|| var("USERPROFILE").filter(|value| !value.is_empty()));
        match home {
            Some(home) => Self::Default(
                PathBuf::from(home)
                    .join(".config")
                    .join("cortex-harness")
                    .join("owner-terms"),
            ),
            None => Self::Nowhere,
        }
    }

    /// How the file is named in a message: never by a path that holds the
    /// home directory.
    fn shown(&self) -> String {
        match self {
            Self::Named(_) => format!("the file {TERMS_FILE} names"),
            Self::Default(_) | Self::Nowhere => DEFAULT_SHOWN.to_owned(),
        }
    }
}

/// What reading the terms gave.
#[derive(Debug)]
pub(crate) enum Loaded {
    Terms(Terms),
    /// No file: a warning, and the push goes on.
    Missing(String),
}

/// Reads the terms. `repositories` gives the folders the file must not be
/// in, the worktree and the main checkout: it is asked only when there is
/// a file.
pub(crate) fn load(
    file: &TermsFile,
    repositories: impl FnOnce() -> Vec<PathBuf>,
) -> Result<Loaded> {
    let path = match file {
        TermsFile::Named(path) | TermsFile::Default(path) => path,
        TermsFile::Nowhere => {
            return Ok(Loaded::Missing(format!(
                "no home directory (HOME or USERPROFILE) to find {DEFAULT_SHOWN} in, and \
                 {TERMS_FILE} is not set"
            )));
        }
    };
    let text = match std::fs::read(path) {
        Ok(bytes) => String::from_utf8(bytes).map_err(|_| {
            format!(
                "{} is not UTF-8 text, so the owner guard cannot read it: the push is refused",
                file.shown()
            )
        })?,
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound
                && matches!(file, TermsFile::Default(_)) =>
        {
            return Ok(Loaded::Missing(format!("there is no {DEFAULT_SHOWN}")));
        }
        Err(error) => {
            return Err(format!(
                "{} cannot be read ({}), so the owner guard cannot run: the push is refused",
                file.shown(),
                error.kind()
            ));
        }
    };
    if let Ok(real) = std::fs::canonicalize(path) {
        // Both sides are canonical, so the comparison is of like with like.
        for repository in repositories() {
            if let Ok(repository) = std::fs::canonicalize(repository)
                && real.starts_with(&repository)
            {
                return Err(format!(
                    "{} is inside the repository, where a commit could take it: move it out. \
                     The push is refused",
                    file.shown()
                ));
            }
        }
    }
    let terms = Terms::parse(&text);
    if terms.is_empty() {
        return Err(format!(
            "{} is there and holds no term, so nothing could be checked: the push is refused",
            file.shown()
        ));
    }
    Ok(Loaded::Terms(terms))
}

/// One line of what git gives a pre-push hook.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Update {
    pub(crate) local_ref: String,
    pub(crate) local: String,
    pub(crate) remote_ref: String,
    pub(crate) remote: String,
}

/// Reads the hook's standard input: `<local ref> <local object> <remote
/// ref> <remote object>`, one a line.
pub(crate) fn parse_updates(text: &str) -> Result<Vec<Update>> {
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
        .map(|(index, line)| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let [local_ref, local, remote_ref, remote] = fields[..] else {
                return Err(format!(
                    "line {} of what git gave the hook is not `<local ref> <object> <remote ref> \
                     <object>`: the push is refused",
                    index + 1
                ));
            };
            let object =
                |name: &str| name.len() >= 40 && name.bytes().all(|byte| byte.is_ascii_hexdigit());
            if !object(local) || !object(remote) {
                return Err(format!(
                    "line {} of what git gave the hook has no object name where one should be: \
                     the push is refused",
                    index + 1
                ));
            }
            Ok(Update {
                local_ref: local_ref.to_owned(),
                local: local.to_owned(),
                remote_ref: remote_ref.to_owned(),
                remote: remote.to_owned(),
            })
        })
        .collect()
}

/// Where a term was found in a push. Its text is never kept.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum Found {
    /// In a commit's own diff: an added line, or a path.
    InCommit { commit: String, finding: Finding },
    /// A commit's message or one of its people.
    Commit(Finding),
    /// The name of a ref the push writes, or of the local one it sends.
    Ref { line: usize, side: &'static str },
    /// An annotated tag's own text: its name, tagger or message.
    Tag { object: String },
}

impl std::fmt::Display for Found {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Found::InCommit {
                commit,
                finding: Finding::Path { file },
            } => write!(
                f,
                "commit {commit}: the path of file {file} of `git show --format= --name-only \
                 --no-renames --diff-filter=d --diff-merges=first-parent {commit}` matches a \
                 protected term"
            ),
            Found::InCommit { commit, finding } => write!(f, "commit {commit}: {finding}"),
            Found::Commit(finding) => write!(f, "{finding}"),
            Found::Ref { line, side } => write!(
                f,
                "the {side} ref name of line {line} of the push matches a protected term"
            ),
            Found::Tag { object } => write!(
                f,
                "tag object {object}: its name, tagger or message matches a protected term"
            ),
        }
    }
}

/// What the updates would add that matches, and how many commits that is.
pub(crate) fn check(
    dir: &Path,
    remote: &str,
    updates: &[Update],
    terms: &Terms,
) -> Result<(Vec<Found>, usize)> {
    let git = |arguments: &[&str]| git(dir, arguments);
    // The remote's branches as this repository last saw them: what they
    // hold is there already. A push to a URL, which names no remote, is
    // held against every remote's.
    let named_remote =
        !remote.is_empty() && git(&["config", "--get", &format!("remote.{remote}.url")]).is_ok();
    let remotes = if named_remote {
        format!("--remotes={remote}")
    } else {
        "--remotes".to_owned()
    };

    let mut found = Vec::new();
    let mut seen = HashSet::new();
    let mut commits = HashSet::new();
    for (index, update) in updates.iter().enumerate() {
        let line = index + 1;
        if is_zero(&update.local) {
            // A deletion sends nothing.
            continue;
        }
        if terms.matches(&update.remote_ref) {
            found.push(Found::Ref {
                line,
                side: "remote",
            });
        }
        if terms.matches(&update.local_ref) {
            found.push(Found::Ref {
                line,
                side: "local",
            });
        }
        if git(&["cat-file", "-t", &update.local])?.trim() == "tag" {
            let text = git(&["cat-file", "tag", &update.local])?;
            if terms.matches(&text) {
                found.push(Found::Tag {
                    object: update.local.chars().take(12).collect(),
                });
            }
        }

        let mut range: Vec<&str> = vec![&update.local, "--not", &remotes];
        if !is_zero(&update.remote)
            && git(&["cat-file", "-e", &format!("{}^{{commit}}", update.remote)]).is_ok()
        {
            range.push(&update.remote);
        }
        // git with these arguments, then the range.
        let run = |first: &[&str]| -> Result<String> {
            let mut all: Vec<&str> = first.to_vec();
            all.extend(range.iter().copied());
            git(&all)
        };

        let list = run(&["rev-list"])?;
        let new: Vec<&str> = list.lines().filter(|line| !line.is_empty()).collect();
        if new.is_empty() {
            continue;
        }
        commits.extend(new.iter().map(|hash| (*hash).to_owned()));

        let log_format = format!("--format={LOG_FORMAT}");
        let log = run(&["log", "--no-show-signature", &log_format])?;
        for finding in scan_commits(terms, &log) {
            found.push(Found::Commit(finding));
        }

        let diffs = run(&[
            "-c",
            "core.quotepath=off",
            "log",
            "--format=%x1e%H",
            "--no-show-signature",
            "--patch",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--no-relative",
            "--no-renames",
            "--text",
            "--unified=0",
            "--src-prefix=a/",
            "--dst-prefix=b/",
            "--diff-merges=first-parent",
        ])?;
        for (commit, diff) in by_commit(&diffs) {
            for finding in scan_diff(terms, &diff) {
                found.push(Found::InCommit {
                    commit: commit.clone(),
                    finding,
                });
            }
        }

        let names = run(&[
            "log",
            "--format=%x1e%H",
            "--no-show-signature",
            "--name-only",
            "-z",
            "--no-relative",
            "--no-renames",
            "--diff-filter=d",
            "--diff-merges=first-parent",
        ])?;
        for (commit, names) in names_by_commit(&names) {
            for finding in scan_paths(terms, &names) {
                found.push(Found::InCommit {
                    commit: commit.clone(),
                    finding,
                });
            }
        }
    }
    found.retain(|finding| seen.insert(finding.clone()));
    Ok((found, commits.len()))
}

/// Whether `line` is a commit's marker: the record separator and a full
/// object name, as `--format=%x1e%H` writes it. A line of a diff begins
/// with a space, `+`, `-`, `@` or a header's word, never with the
/// separator.
fn marker(line: &str) -> Option<String> {
    let hash = line.strip_prefix('\u{1e}')?;
    (hash.len() >= 40 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| hash.chars().take(12).collect())
}

/// `git log --patch` output split into each commit's diff.
fn by_commit(log: &str) -> Vec<(String, String)> {
    let mut commits: Vec<(String, String)> = Vec::new();
    for line in log.split_inclusive('\n') {
        if let Some(commit) = marker(line.trim_end_matches(['\n', '\r'])) {
            commits.push((commit, String::new()));
        } else if let Some((_, diff)) = commits.last_mut() {
            diff.push_str(line);
        }
    }
    commits
}

/// `git log --name-only -z` output split into each commit's names, each
/// ended by a NUL as `git diff --name-only -z` ends them. After a commit's
/// marker git writes a NUL and a line break, then the names.
fn names_by_commit(log: &str) -> Vec<(String, String)> {
    let mut commits: Vec<(String, String)> = Vec::new();
    for field in log.split('\0') {
        if let Some(commit) = marker(field) {
            commits.push((commit, String::new()));
            continue;
        }
        let Some((_, names)) = commits.last_mut() else {
            continue;
        };
        let name = if names.is_empty() {
            field.strip_prefix('\n').unwrap_or(field)
        } else {
            field
        };
        if !name.is_empty() {
            names.push_str(name);
            names.push('\0');
        }
    }
    commits
}

/// The folders a terms file must not be in: the worktree, and the checkout
/// that holds the repository.
fn repositories(dir: &Path) -> Vec<PathBuf> {
    let mut folders = Vec::new();
    if let Ok(top) = git(dir, &["rev-parse", "--show-toplevel"]) {
        folders.push(PathBuf::from(top.trim()));
    }
    if let Ok(common) = git(
        dir,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    ) && let Some(parent) = Path::new(common.trim()).parent()
    {
        folders.push(parent.to_owned());
    }
    folders
}

/// What the module prints, held against the terms first: a message that
/// would print one is withheld. git's own error output can name a path
/// under the home directory, which is a term.
fn screened(terms: Option<&Terms>, text: String) -> String {
    match terms {
        Some(terms) if terms.matches(&text) => {
            "a message that matches a protected term was withheld".to_owned()
        }
        _ => text,
    }
}

/// `owner-guard --pre-push REMOTE [URL]`, with the updates on standard
/// input, and `owner-guard --unpushed [REMOTE]`.
pub(crate) fn run(arguments: &[String]) -> Result<()> {
    let (mode, rest) = arguments.split_first().ok_or_else(usage)?;
    let dir = PathBuf::from(".");
    let file = TermsFile::find(|name| std::env::var_os(name));
    let terms = match load(&file, || repositories(&dir))? {
        Loaded::Terms(terms) => terms,
        Loaded::Missing(why) => {
            eprintln!(
                "warning: owner guard: {why}, so nothing was checked for the owner's personal \
                 details. The owner keeps that file; anyone else may push without it."
            );
            return Ok(());
        }
    };
    let outcome = (|| -> Result<(Vec<Found>, usize)> {
        let (remote, updates) = match mode.as_str() {
            "--pre-push" => {
                let remote = rest.first().ok_or_else(usage)?.clone();
                let mut input = String::new();
                std::io::stdin()
                    .read_to_string(&mut input)
                    .map_err(|error| format!("could not read what git gave the hook: {error}"))?;
                (remote, parse_updates(&input)?)
            }
            "--unpushed" => {
                let remote = rest.first().cloned().unwrap_or_else(|| "origin".to_owned());
                let head = git(&dir, &["rev-parse", "HEAD"])?.trim().to_owned();
                let update = Update {
                    local_ref: "HEAD".to_owned(),
                    local: head,
                    remote_ref: "HEAD".to_owned(),
                    remote: "0".repeat(40),
                };
                (remote, vec![update])
            }
            _ => return Err(usage()),
        };
        check(&dir, &remote, &updates, &terms)
    })();
    let (found, commits) = outcome.map_err(|error| {
        screened(
            Some(&terms),
            format!("owner guard: {error}\nThe push is refused: the check could not run."),
        )
    })?;
    if found.is_empty() {
        eprintln!("owner guard: no protected term in the {commits} commit(s) this push adds");
        return Ok(());
    }
    for finding in &found {
        eprintln!("{}", screened(Some(&terms), finding.to_string()));
    }
    Err(format!(
        "owner guard: {} place(s) in this push match a protected term: the owner's personal \
         details (rule 3). The push is refused. Take them out and rewrite the commits that \
         carry them; never push with --no-verify.",
        found.len()
    ))
}

fn usage() -> String {
    "usage: cargo xtask owner-guard --pre-push REMOTE [URL]   (the git hook; refs on stdin)\n       \
     cargo xtask owner-guard --unpushed [REMOTE]       (HEAD against what REMOTE has)"
        .to_owned()
}

#[cfg(test)]
mod tests;
