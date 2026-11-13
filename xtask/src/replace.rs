//! `cargo xtask replace`: an edit of Rust code that finds its place by
//! tokens, not text, so that it still finds it after `rustfmt` has laid the
//! code out again (`CONTRIBUTING.md`, "Editing from a script").
//!
//! Edit scripts that matched exact text broke whenever a line was wrapped,
//! and a script that fails half way leaves a tree half edited. This uses
//! the matching of `cargo xtask mutate`: white space, comments and a comma
//! before a closing bracket are not tokens. The match must be there exactly
//! once (inside an item, with `--in`), or the command refuses and changes
//! nothing. It prints the lines it changed.

use std::fs;
use std::path::{Path, PathBuf};

use crate::Result;
use crate::mutate::lex;

const USAGE: &str = "\
usage: cargo xtask replace FILE (--from CODE | --from-file PATH)
                                (--to CODE | --to-file PATH)
                                [--in ITEM] [--occurrence N] [--dry-run]

Replaces the code that --from matches, by its tokens, with --to, as text.
White space and comments do not count in the match, so it holds however
rustfmt has laid the code out. The match must be in FILE exactly once, or
inside the item --in names (`Round::spawn`; an impl block answers to every
name in its header); --occurrence N picks one of several, counted from 1.
--dry-run prints what would change and writes nothing. Run `cargo xtask fmt`
afterwards: the new code is put in as it was given.";

/// What `replace` was asked to do.
#[derive(Debug, Default, PartialEq)]
struct Request {
    file: PathBuf,
    from: String,
    to: String,
    within: Option<String>,
    occurrence: Option<usize>,
    dry_run: bool,
}

impl Request {
    fn parse(arguments: &[String]) -> Result<Self> {
        let mut request = Request::default();
        let (mut from, mut to, mut file) = (None, None, None);
        let mut rest = arguments.iter();
        while let Some(argument) = rest.next() {
            let mut value = || {
                rest.next()
                    .cloned()
                    .ok_or_else(|| format!("`{argument}` needs a value\n\n{USAGE}"))
            };
            match argument.as_str() {
                "--from" => from = Some(value()?),
                "--to" => to = Some(value()?),
                "--from-file" => from = Some(read(&value()?)?),
                "--to-file" => to = Some(read(&value()?)?),
                "--in" => request.within = Some(value()?),
                "--occurrence" => {
                    let text = value()?;
                    request.occurrence = Some(
                        text.parse()
                            .ok()
                            .filter(|number| *number > 0)
                            .ok_or_else(|| format!("`--occurrence {text}`: a number from 1"))?,
                    );
                }
                "--dry-run" => request.dry_run = true,
                "--help" | "-h" => return Err(USAGE.to_owned()),
                flag if flag.starts_with('-') => {
                    return Err(format!("unknown flag `{flag}`\n\n{USAGE}"));
                }
                path if file.is_none() => file = Some(PathBuf::from(path)),
                extra => return Err(format!("one file at a time: `{extra}`\n\n{USAGE}")),
            }
        }
        request.file = file.ok_or_else(|| format!("which file?\n\n{USAGE}"))?;
        request.from = from.ok_or_else(|| format!("say what to replace with --from\n\n{USAGE}"))?;
        request.to = to.ok_or_else(|| format!("say what to put there with --to\n\n{USAGE}"))?;
        Ok(request)
    }
}

fn read(path: &str) -> Result<String> {
    let text =
        fs::read_to_string(path).map_err(|error| format!("could not read {path}: {error}"))?;
    // A file written for the purpose ends with a line break that is not
    // part of the code.
    Ok(text.strip_suffix('\n').unwrap_or(&text).to_owned())
}

/// The edited source, and what to print about it.
fn edit(source: &str, request: &Request, shown_as: &str) -> Result<(String, String)> {
    let tokens = lex::tokens(source);
    let inside = request
        .within
        .as_deref()
        .map(|item| format!(" inside `{item}`"))
        .unwrap_or_default();
    let range = lex::locate_in(
        source,
        &tokens,
        request.within.as_deref(),
        &request.from,
        request.occurrence,
    )
    .map_err(|missing| match missing {
        lex::Missing::NothingToFind => "--from has no code in it".to_owned(),
        lex::Missing::NoSuchItem { segment, within } => {
            if within.is_empty() {
                format!("{shown_as} has no item called `{segment}`")
            } else {
                format!("`{within}` in {shown_as} has no item called `{segment}`")
            }
        }
        lex::Missing::NotFound => format!("--from is not in {shown_as}{inside}. Nothing changed."),
        lex::Missing::Ambiguous(lines) => format!(
            "--from is in {shown_as}{inside} {} times, at lines {}. Nothing changed. Make it \
             longer, name the item with --in, or pick one with --occurrence.",
            lines.len(),
            lines
                .iter()
                .map(usize::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        lex::Missing::NoSuchOccurrence { asked, found } => format!(
            "--occurrence {asked}: --from is in {shown_as}{inside} {found} time(s). Nothing \
             changed."
        ),
    })?;
    if lex::same_tokens(&source[range.clone()], &request.to) {
        return Err("--to is the same code as what --from matched. Nothing changed.".to_owned());
    }
    let mut edited = String::with_capacity(source.len() + request.to.len());
    edited.push_str(&source[..range.start]);
    edited.push_str(&request.to);
    edited.push_str(&source[range.end..]);

    // The whole lines the change touches, before and after.
    let first = lex::line_of(source, range.start);
    let last_before = lex::line_of(source, range.end);
    let last_after = lex::line_of(&edited, range.start + request.to.len());
    let lines = |text: &str, from: usize, to: usize, mark: char| {
        text.lines()
            .enumerate()
            .skip(from - 1)
            .take(to + 1 - from)
            .map(|(index, line)| format!("{mark} {:>5}  {line}\n", index + 1))
            .collect::<String>()
    };
    let shown = format!(
        "{shown_as}: lines {first}-{last_before} are now {first}-{last_after}\n{}{}",
        lines(source, first, last_before, '-'),
        lines(&edited, first, last_after, '+'),
    );
    Ok((edited, shown))
}

/// `cargo xtask replace …`
pub(crate) fn run(arguments: &[String]) -> Result<()> {
    let request = Request::parse(arguments)?;
    let path: &Path = &request.file;
    let source = fs::read_to_string(path)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    let (edited, shown) = edit(&source, &request, &path.display().to_string())?;
    print!("{shown}");
    if request.dry_run {
        println!("(a dry run: nothing was written)");
        return Ok(());
    }
    fs::write(path, edited).map_err(|error| format!("could not write {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::{Request, edit};

    /// One function as written by hand, and as `rustfmt` lays it out.
    const BEFORE_FORMAT: &str = "fn budget(tier: Tier, os: Os) -> f64 { match (tier, os) { (Tier::Unit, _) => 20.0, (Tier::System, Os::Windows) => 90.0, _ => 60.0 } }\n";
    const AFTER_FORMAT: &str = "\
fn budget(tier: Tier, os: Os) -> f64 {
    match (tier, os) {
        (Tier::Unit, _) => 20.0,
        // The Windows runners are slower.
        (Tier::System, Os::Windows) => 90.0,
        _ => 60.0,
    }
}
";

    fn request(from: &str, to: &str) -> Request {
        Request {
            from: from.to_owned(),
            to: to.to_owned(),
            ..Request::default()
        }
    }

    #[test]
    fn a_match_holds_before_and_after_rustfmt() {
        let asked = request(
            "(Tier::System, Os::Windows) => 90.0,",
            "(Tier::System, Os::Windows) => 120.0,",
        );
        for source in [BEFORE_FORMAT, AFTER_FORMAT] {
            let (edited, shown) = edit(source, &asked, "budget.rs").unwrap();
            assert!(
                edited.contains("(Tier::System, Os::Windows) => 120.0,"),
                "{edited}"
            );
            assert!(!edited.contains("90.0"), "{edited}");
            // Only the match changed.
            assert_eq!(edited.len(), source.len() + 1, "{edited}");
            assert!(shown.contains("+ "), "{shown}");
        }
        // Laid out across lines in --from too, it still matches.
        let wrapped = request("(Tier::Unit,\n   _)\n  => 20.0", "(Tier::Unit, _) => 25.0");
        let (edited, _) = edit(AFTER_FORMAT, &wrapped, "budget.rs").unwrap();
        assert!(edited.contains("(Tier::Unit, _) => 25.0,"), "{edited}");
    }

    #[test]
    fn the_changed_lines_are_printed() {
        let asked = request("_ => 60.0", "_ => 61.0");
        let (_, shown) = edit(AFTER_FORMAT, &asked, "budget.rs").unwrap();
        assert_eq!(
            shown,
            "budget.rs: lines 6-6 are now 6-6\n-     6          _ => 60.0,\n+     6          _ => 61.0,\n"
        );
    }

    #[test]
    fn a_match_that_is_not_there_once_changes_nothing() {
        let missing = edit(AFTER_FORMAT, &request("=> 75.0", "=> 1.0"), "budget.rs");
        assert!(missing.unwrap_err().contains("is not in budget.rs"));

        let twice = edit(AFTER_FORMAT, &request("Tier::", "Level::"), "budget.rs");
        let error = twice.unwrap_err();
        assert!(error.contains("2 times, at lines 3, 5"), "{error}");

        let mut second = request("Tier::", "Level::");
        second.occurrence = Some(2);
        let (edited, _) = edit(AFTER_FORMAT, &second, "budget.rs").unwrap();
        assert!(
            edited.contains("(Tier::Unit, _)") && edited.contains("(Level::System"),
            "{edited}"
        );

        let mut inside = request("match", "match");
        inside.within = Some("cost".to_owned());
        let error = edit(AFTER_FORMAT, &inside, "budget.rs").unwrap_err();
        assert!(error.contains("no item called `cost`"), "{error}");

        let same = edit(
            AFTER_FORMAT,
            &request("_ => 60.0", "_  =>  60.0"),
            "budget.rs",
        );
        assert!(same.unwrap_err().contains("the same code"));
    }

    #[test]
    fn a_request_is_read() {
        let strings = |words: &[&str]| {
            words
                .iter()
                .map(|word| (*word).to_owned())
                .collect::<Vec<_>>()
        };
        let asked = Request::parse(&strings(&[
            "src/a.rs",
            "--from",
            "x + 1",
            "--to",
            "x + 2",
            "--in",
            "f",
            "--occurrence",
            "2",
            "--dry-run",
        ]))
        .unwrap();
        assert_eq!(asked.file.to_str(), Some("src/a.rs"));
        assert_eq!((asked.from.as_str(), asked.to.as_str()), ("x + 1", "x + 2"));
        assert_eq!(asked.within.as_deref(), Some("f"));
        assert_eq!(asked.occurrence, Some(2));
        assert!(asked.dry_run);
        for (words, expected) in [
            (&["--from", "x", "--to", "y"][..], "which file?"),
            (&["a.rs", "--to", "y"], "--from"),
            (&["a.rs", "--from", "x"], "--to"),
            (
                &["a.rs", "b.rs", "--from", "x", "--to", "y"],
                "one file at a time",
            ),
            (
                &["a.rs", "--from", "x", "--to", "y", "--occurrence", "0"],
                "a number from 1",
            ),
            (&["a.rs", "--from"], "needs a value"),
        ] {
            let error = Request::parse(&strings(words)).unwrap_err();
            assert!(error.contains(expected), "{words:?}: {error}");
        }
    }
}
