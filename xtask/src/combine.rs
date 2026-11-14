//! `cargo xtask combine PR…`: this branch on top of the pull requests that
//! will merge before it, checked as CI checks it (`docs/testing-and-ci.md`,
//! "This branch on top of others").
//!
//! A clean rebase can hide a conflict of meaning: in wave 6 one pull request
//! added a trait method with a default body, and a parallel one wrapped the
//! trait without forwarding it. Each passed alone. This builds the tree the
//! merges will make and runs `cargo xtask ci` on it, before either merges.
//!
//! **Nothing of the session's own is touched.** The work is done in a clone
//! under the build directory (`target/combine/tree`), made with
//! `git clone --shared`, which reads the repository and writes nothing to
//! it: no branch, no ref, no worktree entry, no `FETCH_HEAD`. The pull
//! requests are fetched into the clone. The clone is removed when the run
//! ends, pass or fail, unless `--keep` is given, and any clone a killed run
//! left is removed when the next one starts. Its build directory
//! (`target/combine/target`) is kept, so that the next run builds only what
//! changed.
//!
//! What is combined: the main branch as GitHub has it now, then each pull
//! request's commits since it left the main branch, in the order given, then
//! this branch's commits since it left the main branch: the order a rebase
//! merge of each would make. Uncommitted changes are not in it.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::Result;
use crate::workspace;

const USAGE: &str = "\
usage: cargo xtask combine [--keep] [--no-ci] PR...
       cargo xtask combine --repo URL --head BRANCH [--keep] [--no-ci] PR...

Puts this branch on top of the open pull requests named (by number), in
order, on top of the main branch as GitHub has it, in a clone under the build
directory, and runs `cargo xtask ci` there. Reports the first conflict, or
what ci said. This worktree, its branch and its repository are not touched:
the clone is made with `git clone --shared` and removed at the end.

  --keep    Leave the clone in place (target/combine/tree), to look at.
  --no-ci   Combine and report conflicts, and run nothing.
  --repo URL --head BRANCH
            Take this branch from the repository at URL (pushed), not from
            the checkout: for a copy of the tree with no .git, such as the
            one `rx` builds in.";

/// One thing put on top of the main branch: a pull request, or this branch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Layer {
    /// `#41`, or `this branch`.
    pub(crate) label: String,
    /// The ref to fetch it by from the remote (`pull/41/head`), or `None`
    /// for one already in the clone.
    pub(crate) fetch: Option<String>,
    /// Its head, once known: a ref of the clone.
    pub(crate) head: String,
}

/// How a combination ended.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Every layer applied, and the check passed or failed.
    Checked { passed: bool, applied: Vec<String> },
    /// Every layer applied and nothing was run (`--no-ci`).
    Combined { applied: Vec<String> },
    /// A commit of a layer did not apply.
    Conflict {
        layer: String,
        commit: String,
        files: Vec<String>,
        applied: Vec<String>,
    },
}

/// Where the combining is done.
#[derive(Debug)]
pub(crate) struct Places {
    /// The clone, removed at the end.
    pub(crate) tree: PathBuf,
    /// Where the clone's commits come from: the checkout's repository, or a
    /// URL.
    pub(crate) source: String,
    /// The remote the main branch and the pull requests are fetched from.
    pub(crate) remote: String,
}

/// `git` in `directory`, with the output. An identity of its own: the
/// commits a combination makes never leave the clone.
fn git(directory: &Path, arguments: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=combine",
            "-c",
            "user.email=combine@cortex-harness.invalid",
            "-c",
            "advice.detachedHead=false",
            "-c",
            "core.autocrlf=false",
        ])
        .args(arguments)
        .current_dir(directory)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("could not run git: {error}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    } else {
        Err(format!(
            "git {} failed in {}: {}",
            arguments.join(" "),
            directory.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// Removes a directory and everything in it, if it is there. On Windows git
/// makes its object files read-only, which a plain removal refuses.
pub(crate) fn remove_tree(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    if fs::remove_dir_all(path).is_ok() {
        return Ok(());
    }
    make_writable(path);
    fs::remove_dir_all(path)
        .map_err(|error| format!("could not remove {}: {error}", path.display()))
}

fn make_writable(path: &Path) {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        let mut permissions = metadata.permissions();
        if permissions.readonly() {
            #[expect(
                clippy::permissions_set_readonly_false,
                reason = "a scratch clone's own files, about to be removed"
            )]
            permissions.set_readonly(false);
            let _ = fs::set_permissions(path, permissions);
        }
        if metadata.is_dir()
            && let Ok(entries) = fs::read_dir(path)
        {
            for entry in entries.flatten() {
                make_writable(&entry.path());
            }
        }
    }
}

/// What is run on the combination: whether it passed.
pub(crate) type Check<'a> = &'a mut dyn FnMut(&Path) -> Result<bool>;

/// Makes the clone and puts the layers on top of the main branch. `check`
/// is run in the clone when every layer applied (it is `cargo xtask ci`,
/// or a test's stand-in), unless it is `None`.
pub(crate) fn combine(
    places: &Places,
    layers: &[Layer],
    check: Option<Check<'_>>,
) -> Result<Outcome> {
    remove_tree(&places.tree)?;
    if let Some(parent) = places.tree.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("could not make {}: {error}", parent.display()))?;
    }
    let tree_text = places.tree.to_string_lossy().into_owned();
    let parent = places.tree.parent().unwrap_or(Path::new("."));
    let shared = Path::new(&places.source).is_dir();
    let mut clone = vec!["clone", "--quiet", "--no-checkout"];
    if shared {
        // Reads the repository's objects in place, and writes nothing to it.
        clone.push("--shared");
    }
    clone.extend([places.source.as_str(), tree_text.as_str()]);
    git(parent, &clone)?;
    let tree = &places.tree;

    let mut refspecs = vec!["+refs/heads/main:refs/combine/main".to_owned()];
    for layer in layers {
        if let Some(fetch) = &layer.fetch {
            refspecs.push(format!("+{fetch}:{}", layer.head));
        }
    }
    let mut fetch = vec!["fetch", "--quiet", "--no-tags", places.remote.as_str()];
    fetch.extend(refspecs.iter().map(String::as_str));
    git(tree, &fetch)?;
    git(
        tree,
        &["checkout", "--quiet", "--detach", "refs/combine/main"],
    )?;

    let mut applied = Vec::new();
    for layer in layers {
        let since = git(tree, &["merge-base", "refs/combine/main", &layer.head])?;
        let range = format!("{since}..{}", layer.head);
        let commits = git(tree, &["rev-list", "--reverse", "--no-merges", &range])?;
        for commit in commits.lines().filter(|line| !line.is_empty()) {
            let picked = git(
                tree,
                &[
                    "cherry-pick",
                    "--allow-empty",
                    "--keep-redundant-commits",
                    commit,
                ],
            );
            if picked.is_err() {
                let files = git(tree, &["diff", "--name-only", "--diff-filter=U"])?;
                let subject = git(tree, &["log", "-1", "--format=%h %s", commit])?;
                let _ = git(tree, &["cherry-pick", "--abort"]);
                return Ok(Outcome::Conflict {
                    layer: layer.label.clone(),
                    commit: subject,
                    files: files.lines().map(str::to_owned).collect(),
                    applied,
                });
            }
        }
        applied.push(format!(
            "{} ({} commit(s))",
            layer.label,
            commits.lines().filter(|line| !line.is_empty()).count()
        ));
    }
    Ok(match check {
        Some(check) => Outcome::Checked {
            passed: check(tree)?,
            applied,
        },
        None => Outcome::Combined { applied },
    })
}

/// What `cargo xtask combine` was asked for.
#[derive(Debug, PartialEq, Eq)]
struct Request {
    numbers: Vec<u32>,
    keep: bool,
    ci: bool,
    repo: Option<String>,
    head: Option<String>,
}

impl Request {
    fn parse(arguments: &[String]) -> Result<Self> {
        let mut request = Request {
            numbers: Vec::new(),
            keep: false,
            ci: true,
            repo: None,
            head: None,
        };
        let mut words = arguments.iter();
        while let Some(word) = words.next() {
            match word.as_str() {
                "--keep" => request.keep = true,
                "--no-ci" => request.ci = false,
                "--repo" | "--head" => {
                    let value = words
                        .next()
                        .ok_or_else(|| format!("`{word}` needs a value\n\n{USAGE}"))?
                        .clone();
                    if value.starts_with('-') {
                        return Err(format!("`{word} {value}`: not a value"));
                    }
                    if word == "--repo" {
                        request.repo = Some(value);
                    } else {
                        request.head = Some(value);
                    }
                }
                "--help" | "-h" => return Err(USAGE.to_owned()),
                number => request.numbers.push(
                    number
                        .trim_start_matches('#')
                        .parse()
                        .ok()
                        .filter(|number| *number > 0)
                        .ok_or_else(|| {
                            format!("`{number}` is not a pull request's number\n\n{USAGE}")
                        })?,
                ),
            }
        }
        if request.numbers.is_empty() {
            return Err(format!(
                "name the pull requests to put this branch on\n\n{USAGE}"
            ));
        }
        if request.repo.is_some() != request.head.is_some() {
            return Err("`--repo` and `--head` go together".to_owned());
        }
        Ok(request)
    }
}

/// The layers for pull requests `numbers` and then this branch, whose head
/// is `head` in the clone.
fn layers(numbers: &[u32], head: &str) -> Vec<Layer> {
    let mut layers: Vec<Layer> = numbers
        .iter()
        .map(|number| Layer {
            label: format!("#{number}"),
            fetch: Some(format!("refs/pull/{number}/head")),
            head: format!("refs/combine/pr-{number}"),
        })
        .collect();
    layers.push(Layer {
        label: "this branch".to_owned(),
        fetch: None,
        head: head.to_owned(),
    });
    layers
}

/// The text the run ends with.
pub(crate) fn describe(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Checked { passed, applied } => format!(
            "Combined: main, then {}.\n`cargo xtask ci` on the combination {}.",
            applied.join(", then "),
            if *passed {
                "PASSED"
            } else {
                "FAILED: its output is above"
            }
        ),
        Outcome::Combined { applied } => format!(
            "Combined: main, then {}. Nothing was run (--no-ci).",
            applied.join(", then ")
        ),
        Outcome::Conflict {
            layer,
            commit,
            files,
            applied,
        } => {
            let before = if applied.is_empty() {
                "main".to_owned()
            } else {
                format!("main, then {}", applied.join(", then "))
            };
            format!(
                "CONFLICT: {layer} does not apply on top of {before}.\nIts commit {commit} \
                 conflicts in:\n{}",
                files
                    .iter()
                    .map(|file| format!("  {file}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        }
    }
}

/// Warns when a pull request is not open, through `gh`; says so when `gh`
/// cannot be asked.
fn say_whether_open(number: u32) {
    let asked = Command::new("gh")
        .args([
            "pr",
            "view",
            &number.to_string(),
            "--json",
            "state",
            "--jq",
            ".state",
        ])
        .stdin(Stdio::null())
        .output();
    match asked {
        Ok(output) if output.status.success() => {
            let state = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            if state != "OPEN" {
                println!("warning: #{number} is {state}, not open");
            }
        }
        _ => println!("note: gh could not say whether #{number} is open; it is fetched anyway"),
    }
}

/// `cargo xtask combine …`
pub(crate) fn run(arguments: &[String]) -> Result<()> {
    let request = Request::parse(arguments)?;
    let root = workspace::root();
    let target = workspace::metadata(&root)?.target_directory;
    let base = target.join("combine");
    let (source, remote, head) = match (&request.repo, &request.head) {
        (Some(repo), Some(_)) => (repo.clone(), repo.clone(), "refs/combine/head".to_owned()),
        _ => {
            if !root.join(".git").exists() {
                return Err(
                    "this copy of the tree has no .git (an rx copy?): give the pushed branch \
                     with --repo URL --head BRANCH"
                        .to_owned(),
                );
            }
            let dirty = git(&root, &["status", "--porcelain", "--untracked-files=no"])?;
            if !dirty.is_empty() {
                println!("note: uncommitted changes are not in the combination: commit them first");
            }
            (
                root.to_string_lossy().into_owned(),
                git(&root, &["remote", "get-url", "origin"])?,
                git(&root, &["rev-parse", "HEAD"])?,
            )
        }
    };
    for number in &request.numbers {
        say_whether_open(*number);
    }
    let mut layers = layers(&request.numbers, &head);
    if let Some(branch) = &request.head {
        let last = layers.len() - 1;
        layers[last].fetch = Some(format!("refs/heads/{branch}"));
    }
    let places = Places {
        tree: base.join("tree"),
        source,
        remote,
    };
    let build = base.join("target");
    let mut ci = |tree: &Path| -> Result<bool> {
        println!("$ cargo xtask ci   (in {})", tree.display());
        let status = Command::new(workspace::cargo())
            .args(["xtask", "ci"])
            .current_dir(tree)
            .env("CARGO_TARGET_DIR", &build)
            .env_remove("CARGO_BUILD_TARGET_DIR")
            .status()
            .map_err(|error| format!("could not run cargo: {error}"))?;
        Ok(status.success())
    };
    let check: Option<Check<'_>> = if request.ci { Some(&mut ci) } else { None };
    let outcome = combine(&places, &layers, check);
    let kept = if request.keep {
        format!("The clone is kept at {}.", places.tree.display())
    } else {
        remove_tree(&places.tree)?;
        "The clone is removed; its build directory is kept.".to_owned()
    };
    let outcome = outcome?;
    println!("\n{}\n{kept}", describe(&outcome));
    match outcome {
        Outcome::Checked { passed: true, .. } | Outcome::Combined { .. } => Ok(()),
        Outcome::Checked { passed: false, .. } => Err("ci failed on the combination".to_owned()),
        Outcome::Conflict { .. } => Err("the combination has a conflict".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use super::{Outcome, Places, Request, combine, describe, git, layers, remove_tree};

    fn strings(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| (*word).to_owned()).collect()
    }

    #[test]
    fn a_request_is_read() {
        assert_eq!(
            Request::parse(&strings(&["41", "#42", "--keep"])).unwrap(),
            Request {
                numbers: vec![41, 42],
                keep: true,
                ci: true,
                repo: None,
                head: None,
            }
        );
        for (words, expected) in [
            (&[][..], "name the pull requests"),
            (&["x"], "not a pull request's number"),
            (&["0"], "not a pull request's number"),
            (&["--repo", "u", "41"], "go together"),
            (&["--head", "--keep", "41"], "not a value"),
        ] {
            let error = Request::parse(&strings(words)).unwrap_err();
            assert!(error.contains(expected), "{words:?}: {error}");
        }
    }

    /// A small repository standing for GitHub: a main branch with a file,
    /// two pull requests as `refs/pull/N/head`, and a checkout of it on a
    /// branch of the session's own with a commit.
    struct Fixture {
        root: PathBuf,
        remote: PathBuf,
        session: PathBuf,
    }

    fn write(at: &Path, file: &str, text: &str) {
        fs::write(at.join(file), text).unwrap();
    }

    fn commit(at: &Path, message: &str) {
        git(at, &["add", "-A"]).unwrap();
        git(at, &["commit", "--quiet", "-m", message]).unwrap();
    }

    impl Fixture {
        /// `second` is what the second pull request writes over the line the
        /// first one changed: the same change, or another.
        fn new(name: &str, second: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("xtask-combine-{name}-{}", std::process::id()));
            remove_tree(&root).unwrap();
            let remote = root.join("remote");
            fs::create_dir_all(&remote).unwrap();
            git(&remote, &["init", "--quiet", "--initial-branch", "main"]).unwrap();
            write(&remote, "a.txt", "one\ntwo\nthree\n");
            write(&remote, "b.txt", "b\n");
            commit(&remote, "Start");
            for (number, file, text) in [(1, "a.txt", "one\nTWO\nthree\n"), (2, "a.txt", second)] {
                git(
                    &remote,
                    &["checkout", "--quiet", "-b", &format!("pr-{number}"), "main"],
                )
                .unwrap();
                write(&remote, file, text);
                commit(&remote, &format!("Change {file} in pull request {number}"));
                git(
                    &remote,
                    &["update-ref", &format!("refs/pull/{number}/head"), "HEAD"],
                )
                .unwrap();
            }
            git(&remote, &["checkout", "--quiet", "main"]).unwrap();
            let session = root.join("session");
            git(
                &root,
                &["clone", "--quiet", &remote.to_string_lossy(), "session"],
            )
            .unwrap();
            git(&session, &["checkout", "--quiet", "-b", "mine"]).unwrap();
            write(&session, "c.txt", "mine\n");
            commit(&session, "Add c.txt on the session's branch");
            Self {
                root,
                remote,
                session,
            }
        }

        fn places(&self) -> Places {
            Places {
                tree: self.root.join("combine").join("tree"),
                source: self.session.to_string_lossy().into_owned(),
                remote: self.remote.to_string_lossy().into_owned(),
            }
        }

        /// The session's repository as a test can see it: its branch, its
        /// head, its refs and its worktrees.
        fn session_state(&self) -> String {
            [
                git(&self.session, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap(),
                git(&self.session, &["rev-parse", "HEAD"]).unwrap(),
                git(&self.session, &["for-each-ref"]).unwrap(),
                git(&self.session, &["worktree", "list", "--porcelain"]).unwrap(),
                git(&self.session, &["status", "--porcelain"]).unwrap(),
            ]
            .join("\n")
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = remove_tree(&self.root);
        }
    }

    #[test]
    fn sys_two_pull_requests_that_agree_combine_and_the_check_sees_all_three() {
        let fixture = Fixture::new("agree", "one\ntwo\nthree\nfour\n");
        // The second adds a line after the one the first changed: no conflict.
        let before = fixture.session_state();
        let head = git(&fixture.session, &["rev-parse", "HEAD"]).unwrap();
        let mut seen = String::new();
        let mut check = |tree: &Path| -> crate::Result<bool> {
            for file in ["a.txt", "c.txt"] {
                seen.push_str(&fs::read_to_string(tree.join(file)).unwrap());
            }
            Ok(true)
        };
        let outcome =
            combine(&fixture.places(), &layers(&[1, 2], &head), Some(&mut check)).unwrap();

        assert_eq!(
            outcome,
            Outcome::Checked {
                passed: true,
                applied: vec![
                    "#1 (1 commit(s))".to_owned(),
                    "#2 (1 commit(s))".to_owned(),
                    "this branch (1 commit(s))".to_owned()
                ]
            }
        );
        assert_eq!(seen.replace("\r\n", "\n"), "one\nTWO\nthree\nfour\nmine\n");
        assert_eq!(
            fixture.session_state(),
            before,
            "the session's repository changed"
        );
        remove_tree(&fixture.places().tree).unwrap();
        assert!(!fixture.places().tree.exists());
    }

    #[test]
    fn sys_two_pull_requests_that_conflict_name_the_layer_the_commit_and_the_file() {
        let fixture = Fixture::new("conflict", "one\nzwei\nthree\n");
        let before = fixture.session_state();
        let head = git(&fixture.session, &["rev-parse", "HEAD"]).unwrap();
        let mut ran = false;
        let mut check = |_: &Path| -> crate::Result<bool> {
            ran = true;
            Ok(true)
        };
        let outcome =
            combine(&fixture.places(), &layers(&[1, 2], &head), Some(&mut check)).unwrap();

        let Outcome::Conflict {
            layer,
            commit,
            files,
            applied,
        } = &outcome
        else {
            panic!("{outcome:?}");
        };
        assert_eq!(layer, "#2");
        assert!(
            commit.ends_with("Change a.txt in pull request 2"),
            "{commit}"
        );
        assert_eq!(files, &["a.txt"]);
        assert_eq!(applied, &["#1 (1 commit(s))"]);
        assert!(!ran, "the check ran on a combination that did not apply");
        let said = describe(&outcome);
        assert!(
            said.contains("CONFLICT: #2 does not apply on top of main, then #1"),
            "{said}"
        );
        assert_eq!(
            fixture.session_state(),
            before,
            "the session's repository changed"
        );
    }

    #[test]
    fn sys_a_run_from_a_pushed_branch_takes_it_from_the_remote() {
        let fixture = Fixture::new("pushed", "one\ntwo\nthree\nfour\n");
        git(&fixture.session, &["push", "--quiet", "origin", "mine"]).unwrap();
        let places = Places {
            source: fixture.remote.to_string_lossy().into_owned(),
            ..fixture.places()
        };
        let mut all = layers(&[2], "refs/combine/head");
        all[1].fetch = Some("refs/heads/mine".to_owned());
        let outcome = combine(&places, &all, None).unwrap();
        assert_eq!(
            outcome,
            Outcome::Combined {
                applied: vec![
                    "#2 (1 commit(s))".to_owned(),
                    "this branch (1 commit(s))".to_owned()
                ]
            }
        );
    }
}
