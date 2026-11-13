//! Every link inside the repository leads somewhere.
//!
//! The docs are the specification, and they are read by following links:
//! from the build plan to an architecture page, from a step's record to a
//! heading of another page. A page that is split, or a heading that is
//! reworded, breaks links in files the change never opened. This finds
//! them:
//!
//! - a **relative link** in a Markdown file names a file or a directory of
//!   the repository;
//! - a link with a **`#fragment`** to a Markdown file names a heading of
//!   that file, by the name GitHub gives the heading;
//! - a **`docs/….md` path written in prose, in configuration or in a
//!   comment of the code** names a file that is there. In Rust source only
//!   what follows `//` on a line is read: that is where the code cites the
//!   page it follows, and a string there is as likely to be a test's
//!   made-up file.
//!
//! Links to other sites are not followed: nothing here uses the network.
//!
//! A heading's name is worked out as GitHub does it: the heading's text in
//! lower case, with everything but letters, digits, `-` and `_` taken out
//! and each space made a `-`; and a second heading with the same name gets
//! `-1` after it, a third `-2`.

use std::collections::{BTreeMap, BTreeSet};

use super::Violation;

/// The files whose text is searched for `docs/….md` paths, by their ending.
const WITH_PATHS: [&str; 7] = [".rs", ".toml", ".yml", ".yaml", ".md", ".breaks", ".txt"];

/// Whether a file's text is read by this rule.
pub(super) fn is_read(file: &str) -> bool {
    WITH_PATHS.iter().any(|ending| file.ends_with(ending))
}

/// Checks the links of every Markdown file in `texts`, and the `docs/`
/// paths named in every file of `texts`. `files` is every file of the
/// repository, relative to its root, with `/`.
pub(crate) fn check(texts: &BTreeMap<String, String>, files: &BTreeSet<String>) -> Vec<Violation> {
    let mut violations = Vec::new();
    // The headings of a page, worked out when a link first asks for them.
    let mut anchors: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();

    for (file, text) in texts {
        let markdown = file.ends_with(".md");
        if markdown {
            for link in links(text) {
                let Some(problem) = judge(file, &link.target, texts, files, &mut anchors) else {
                    continue;
                };
                violations.push(Violation::new(file, Some(link.line), problem));
            }
        }
        if is_read(file) && !is_fixture(file) {
            for (line, path) in named_docs(text, file.ends_with(".rs")) {
                if !files.contains(&path) {
                    violations.push(Violation::new(
                        file,
                        Some(line),
                        format!(
                            "names `{path}`, which is not in the repository.\n    A page that \
                             is moved or renamed takes the places that name it with it. Write \
                             the path the page has now."
                        ),
                    ));
                }
            }
        }
    }
    violations
}

/// Test data may name pages that were never there.
fn is_fixture(file: &str) -> bool {
    file.split('/').any(|part| part == "fixtures")
}

/// What is wrong with a link's target, if anything.
fn judge<'a>(
    from: &str,
    target: &str,
    texts: &'a BTreeMap<String, String>,
    files: &BTreeSet<String>,
    anchors: &mut BTreeMap<&'a str, BTreeSet<String>>,
) -> Option<String> {
    if has_scheme(target) {
        return None;
    }
    let (path, fragment) = match target.split_once('#') {
        Some((path, fragment)) => (path, Some(fragment)),
        None => (target, None),
    };
    // A query string is not part of the file's name.
    let path = path.split('?').next().unwrap_or(path);
    let resolved = if path.is_empty() {
        from.to_owned()
    } else {
        match resolve(from, &percent_decoded(path)) {
            Some(resolved) => resolved,
            None => {
                return Some(format!(
                    "the link `{target}` leads out of the repository.\n    A link between \
                     pages is relative and stays inside the repository."
                ));
            }
        }
    };
    let is_file = files.contains(&resolved);
    let is_directory = resolved.is_empty()
        || files
            .range(format!("{resolved}/")..)
            .next()
            .is_some_and(|file| file.starts_with(&format!("{resolved}/")));
    if !is_file && !is_directory {
        return Some(format!(
            "the link `{target}` leads to `{resolved}`, which is not in the repository.\n    \
             The page was moved, renamed or never written. Links are relative to the file \
             they are in."
        ));
    }
    let fragment = fragment.filter(|fragment| !fragment.is_empty())?;
    if !resolved.ends_with(".md") {
        return None;
    }
    let (page, text) = texts.get_key_value(&resolved)?;
    let known = anchors
        .entry(page.as_str())
        .or_insert_with(|| heading_anchors(text));
    if known.contains(&percent_decoded(fragment)) {
        return None;
    }
    Some(format!(
        "the link `{target}` names a heading that `{resolved}` does not have.\n    A heading's \
         name is its text in lower case, without punctuation, with `-` for each space: \
         \"Where the five minutes go\" is `#where-the-five-minutes-go`. The heading was \
         reworded or moved to another page."
    ))
}

/// Whether a link's target begins with a scheme: `https:`, `mailto:`.
fn has_scheme(target: &str) -> bool {
    let Some((scheme, _)) = target.split_once(':') else {
        return false;
    };
    let mut characters = scheme.chars();
    characters
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic())
        && characters.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
}

/// The file a relative path names, from the file it is written in. A path
/// that begins with `/` is from the repository's root, as GitHub reads it.
/// `None` when the path climbs out of the repository.
fn resolve(from: &str, path: &str) -> Option<String> {
    let mut parts: Vec<&str> = if path.starts_with('/') {
        Vec::new()
    } else {
        let mut directory: Vec<&str> = from.split('/').collect();
        directory.pop();
        directory
    };
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            name => parts.push(name),
        }
    }
    Some(parts.join("/"))
}

/// Undoes `%20` and the like. What is not a well-formed escape is kept.
fn percent_decoded(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        let escape = (bytes[at] == b'%')
            .then(|| text.get(at + 1..at + 3))
            .flatten()
            .and_then(|digits| u8::from_str_radix(digits, 16).ok());
        match escape {
            Some(byte) => {
                decoded.push(byte);
                at += 3;
            }
            None => {
                decoded.push(bytes[at]);
                at += 1;
            }
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

/// One link of a Markdown file.
#[derive(Debug, PartialEq, Eq)]
struct Link {
    line: usize,
    target: String,
}

/// The lines of a Markdown file that are prose: not inside a fenced code
/// block. Each with its number, counted from 1.
fn prose_lines(text: &str) -> Vec<(usize, &str)> {
    let mut lines = Vec::new();
    // The fence that opened the block being skipped: its character and length.
    let mut fence: Option<(char, usize)> = None;
    for (index, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();
        let marker = trimmed.chars().next().filter(|c| matches!(c, '`' | '~'));
        let length = marker.map_or(0, |marker| {
            trimmed.chars().take_while(|c| *c == marker).count()
        });
        // "```rust``` and more" is a code span on a line of prose: a fence of
        // backticks has no backtick after it on its line.
        let opens = length >= 3 && !(marker == Some('`') && trimmed[length..].contains('`'));
        match (fence, marker) {
            (None, Some(marker)) if opens => fence = Some((marker, length)),
            (Some((open, open_length)), Some(marker))
                if marker == open
                    && length >= open_length
                    && trimmed.trim_start_matches(marker).trim().is_empty() =>
            {
                fence = None;
            }
            (None, _) => lines.push((index + 1, line)),
            (Some(_), _) => {}
        }
    }
    lines
}

/// A line without its code spans: what is between backticks is not prose,
/// and a link written there is an example of one.
fn without_code_spans(line: &str) -> String {
    let mut kept = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(start) = rest.find('`') {
        kept.push_str(&rest[..start]);
        let ticks = rest[start..].chars().take_while(|c| *c == '`').count();
        let after = &rest[start + ticks..];
        match after.find(&"`".repeat(ticks)) {
            Some(end) => rest = &after[end + ticks..],
            // An opening mark with no closing one is a backtick, and no span.
            None => {
                kept.push_str(&rest[start..start + ticks]);
                rest = after;
            }
        }
    }
    kept.push_str(rest);
    kept
}

/// A line without the HTML comments that begin and end on it.
fn without_comments(line: &str) -> String {
    let mut kept = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(start) = rest.find("<!--") {
        kept.push_str(&rest[..start]);
        rest = rest[start..]
            .find("-->")
            .map_or("", |end| &rest[start + end + 3..]);
    }
    kept.push_str(rest);
    kept
}

/// The links of a Markdown file: `[text](target)`, `![alt](target)`, and
/// the definition of a reference, `[name]: target`.
fn links(text: &str) -> Vec<Link> {
    let mut found = Vec::new();
    for (number, line) in prose_lines(text) {
        let line = without_comments(&without_code_spans(line));
        let mut rest = line.as_str();
        while let Some(start) = rest.find("](") {
            let after = &rest[start + 2..];
            // The target runs to the closing bracket, and may hold brackets
            // of its own in pairs.
            let mut depth = 0usize;
            let mut end = None;
            for (at, character) in after.char_indices() {
                match character {
                    '(' => depth += 1,
                    ')' if depth == 0 => {
                        end = Some(at);
                        break;
                    }
                    ')' => depth -= 1,
                    _ => {}
                }
            }
            let Some(end) = end else {
                break;
            };
            // `(target "title")`, and `(<target with spaces>)`.
            let inside = after[..end].trim();
            let target = match inside.strip_prefix('<') {
                Some(angled) => angled.split('>').next().unwrap_or(angled),
                None => {
                    let mut words = inside.split_whitespace();
                    let target = words.next().unwrap_or("");
                    // After the target there is a title in quotes or
                    // nothing. `[a](two words)` is not a link.
                    match words.next() {
                        Some(title) if !title.starts_with(['"', '\'', '(']) => "",
                        _ => target,
                    }
                }
            };
            if !target.is_empty() {
                found.push(Link {
                    line: number,
                    target: target.to_owned(),
                });
            }
            rest = &after[end..];
        }
        // `[name]: target`, at the start of a line.
        let trimmed = line.trim_start();
        if let Some(definition) = trimmed.strip_prefix('[')
            && let Some((name, target)) = definition.split_once("]:")
            && !name.contains(']')
            // `[^1]: A note.` is a footnote, and its text is not a target.
            && !name.starts_with('^')
            && let Some(target) = target.split_whitespace().next()
        {
            found.push(Link {
                line: number,
                target: target.trim_matches(['<', '>']).to_owned(),
            });
        }
    }
    found
}

/// The names GitHub gives the headings of a Markdown file, and the names
/// set by hand with `<a id="…">` or `<a name="…">`.
fn heading_anchors(text: &str) -> BTreeSet<String> {
    let mut anchors = BTreeSet::new();
    // How many times each name has been given out. A name that is taken
    // gets the next number that is free, and the numbered name is taken
    // too: with a heading `x-1` between two `x`, the second `x` is `x-2`.
    let mut given: BTreeMap<String, usize> = BTreeMap::new();
    let mut add = |heading: &str| {
        let name = slug(heading);
        let mut numbered = name.clone();
        while given.contains_key(&numbered) {
            let count = given.entry(name.clone()).or_insert(0);
            *count += 1;
            numbered = format!("{name}-{count}");
        }
        given.insert(numbered.clone(), 0);
        anchors.insert(numbered);
    };
    let lines = prose_lines(text);
    for (index, (_, line)) in lines.iter().enumerate() {
        // A heading inside a quotation is a heading.
        let trimmed = line.trim_start().trim_start_matches(['>', ' ']);
        let hashes = trimmed.chars().take_while(|c| *c == '#').count();
        if (1..=6).contains(&hashes) && trimmed[hashes..].starts_with([' ', '\t']) {
            // `## Heading ##` is the heading `Heading`.
            add(trimmed[hashes..].trim().trim_end_matches('#').trim_end());
            continue;
        }
        // A line of `=` or `-` under a line of text makes that line a heading.
        let underline = !trimmed.is_empty()
            && (trimmed.trim_end().chars().all(|c| c == '=')
                || trimmed.trim_end().chars().all(|c| c == '-'));
        if underline
            && index > 0
            && let Some((_, above)) = lines.get(index - 1)
            && !above.trim().is_empty()
            && !above.trim_start().starts_with(['#', '|', '-', '*', '>'])
        {
            add(above.trim());
        }
    }
    for (_, line) in &lines {
        for attribute in ["id=\"", "name=\""] {
            let mut rest = *line;
            while let Some(start) = rest.find(attribute) {
                let after = &rest[start + attribute.len()..];
                if rest[..start].trim_end().ends_with("<a")
                    && let Some(end) = after.find('"')
                {
                    anchors.insert(after[..end].to_owned());
                }
                rest = after;
            }
        }
    }
    anchors
}

/// The name GitHub gives a heading, before it is numbered.
fn slug(heading: &str) -> String {
    // What a reader sees: a link is its text, and an image nothing.
    let mut shown = String::with_capacity(heading.len());
    let mut rest = heading;
    while let Some(start) = rest.find("](") {
        let before = &rest[..start];
        match before.rfind('[') {
            Some(open) => {
                shown.push_str(before[..open].trim_end_matches('!'));
                shown.push_str(&before[open + 1..]);
            }
            None => shown.push_str(before),
        }
        let after = &rest[start + 2..];
        rest = after.find(')').map_or("", |end| &after[end + 1..]);
    }
    shown.push_str(rest);

    let mut name = String::with_capacity(shown.len());
    for character in shown.chars() {
        if character.is_alphanumeric() || matches!(character, '-' | '_') {
            name.extend(character.to_lowercase());
        } else if character == ' ' {
            name.push('-');
        }
    }
    name
}

/// The comment on a line of Rust source: what follows the first `//` that
/// is not inside a string. A `//` with an odd number of quotation marks
/// before it on its line is inside one (`"https://…"`).
fn comment_of(line: &str) -> Option<&str> {
    let mut from = 0;
    while let Some(found) = line[from..].find("//") {
        let at = from + found;
        let before = &line[..at];
        let quotes = before.matches('"').count() - before.matches("\\\"").count();
        if quotes.is_multiple_of(2) {
            return Some(&line[at + 2..]);
        }
        from = at + 2;
    }
    None
}

/// The `docs/….md` paths written in a text, each with its line: a path
/// from the repository's root, as code and prose write them. A path with a
/// placeholder in it (`docs/steps/<NN>/ci.md`) is not one. With
/// `comments_only`, for Rust source, only what follows the first `//` of a
/// line is read: a comment on a line of its own or after code. A block
/// comment is not read.
fn named_docs(text: &str, comments_only: bool) -> Vec<(usize, String)> {
    let is_path = |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '/' | '-');
    let mut found = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let line = if comments_only {
            match comment_of(line) {
                Some(comment) => comment,
                None => continue,
            }
        } else {
            line
        };
        let mut rest = line;
        while let Some(start) = rest.find("docs/") {
            let before = rest[..start].chars().next_back();
            let after = &rest[start..];
            let length = after.find(|c| !is_path(c)).unwrap_or(after.len());
            let path = after[..length].trim_end_matches('.');
            // Part of a longer path (`../docs/x.md`, `crates/a/docs/x.md`)
            // is another file's, and a relative link's own check.
            let whole = before.is_none_or(|c| !is_path(c));
            let placeholder = after[length..].starts_with(['<', '{', '*', '…']);
            if whole && !placeholder && path.ends_with(".md") {
                found.push((index + 1, path.to_owned()));
            }
            rest = &after[length.max(1)..];
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::{Link, check, heading_anchors, links, named_docs, slug};

    /// The violations for a repository of these files, as text.
    fn found(files: &[(&str, &str)]) -> Vec<String> {
        let texts: BTreeMap<String, String> = files
            .iter()
            .map(|(file, text)| ((*file).to_owned(), (*text).to_owned()))
            .collect();
        let names: BTreeSet<String> = texts.keys().cloned().collect();
        check(&texts, &names)
            .into_iter()
            .map(|violation| violation.to_string())
            .collect()
    }

    fn assert_one(found: &[String], needles: &[&str]) {
        assert_eq!(found.len(), 1, "{found:#?}");
        for needle in needles {
            assert!(
                found[0].contains(needle),
                "`{needle}` is not in:\n{}",
                found[0]
            );
        }
    }

    const PLAN: &str = "# The build plan\n\n## Step 10: editing tools and the plan\n\nText.\n";

    #[test]
    fn links_that_lead_somewhere_pass() {
        let page = "\
See [the plan](build-plan.md), [step 10](build-plan.md#step-10-editing-tools-and-the-plan),
[the steps](steps/), [this page](#here), [up](../README.md \"The front page\"),
[from the root](/docs/build-plan.md), [a site](https://example.org/x.md#nowhere),
[mail](mailto:someone@example.org) and ![a picture](../assets/logo.png).

## Here

[plan]: build-plan.md#the-build-plan
";
        assert_eq!(
            found(&[
                ("docs/page.md", page),
                ("docs/build-plan.md", PLAN),
                ("docs/steps/07/ci.md", "# After step 7\n"),
                ("README.md", "# Cortex-Harness\n"),
                ("assets/logo.png", ""),
            ]),
            [] as [&str; 0]
        );
    }

    #[test]
    fn a_link_to_a_file_that_is_not_there_is_found_with_its_line() {
        let found = found(&[("docs/page.md", "# Page\n\nSee [the plan](build-plan.md).\n")]);

        assert_one(
            &found,
            &[
                "docs/page.md:3:",
                "`build-plan.md` leads to `docs/build-plan.md`, which is not in the repository",
            ],
        );
    }

    #[test]
    fn a_link_is_relative_to_the_file_it_is_in() {
        // From a step's record, the plan is two directories up.
        let record = "[the plan](build-plan.md) and [the plan](../../build-plan.md)\n";

        let found = found(&[
            ("docs/steps/07/answered.md", record),
            ("docs/build-plan.md", PLAN),
        ]);

        assert_one(&found, &["`docs/steps/07/build-plan.md`"]);
    }

    #[test]
    fn a_link_to_a_heading_that_is_not_there_is_found() {
        let found = found(&[
            (
                "docs/page.md",
                "[step 10](build-plan.md#step-10-editing-tools)\n[here](#nowhere)\n",
            ),
            ("docs/build-plan.md", PLAN),
        ]);

        assert_eq!(found.len(), 2, "{found:#?}");
        assert!(
            found[0].contains(
                "docs/page.md:1: the link `build-plan.md#step-10-editing-tools` names a heading \
                 that `docs/build-plan.md` does not have"
            ),
            "{}",
            found[0]
        );
        assert!(
            found[1].contains("docs/page.md:2: the link `#nowhere`"),
            "{}",
            found[1]
        );
    }

    #[test]
    fn a_link_that_climbs_out_of_the_repository_is_found() {
        let found = found(&[("README.md", "[elsewhere](../other/README.md)\n")]);

        assert_one(&found, &["leads out of the repository"]);
    }

    #[test]
    fn what_is_in_code_is_not_a_link() {
        let page = "\
A link is written `[text](missing.md)`, or with more marks ``[a](`b`.md)``.

```text
[in a block](missing.md)
## Not a heading
```

~~~~
[in another](missing.md)
```
[still in it](missing.md)
~~~~

[a real one](#not-a-heading)
";
        let found = found(&[("docs/page.md", page)]);

        // The one link outside the code, to a heading that is only in a block.
        assert_one(&found, &["docs/page.md:14:", "`#not-a-heading`"]);
    }

    #[test]
    fn links_are_read_in_every_form() {
        let text = "\
[a](one.md) and [b](two.md \"Two\") and [c](<three four.md>) and ![d](five.png)
[e](six_(disambiguation).md) and [empty]() and [f]: not-a-definition.md
  [g]: seven.md
[h]: <eight.md> \"Eight\"
";
        assert_eq!(
            links(text),
            [
                (1, "one.md"),
                (1, "two.md"),
                (1, "three four.md"),
                (1, "five.png"),
                (2, "six_(disambiguation).md"),
                (3, "seven.md"),
                (4, "eight.md"),
            ]
            .map(|(line, target)| Link {
                line,
                target: target.to_owned()
            })
        );
    }

    #[test]
    fn what_only_looks_like_a_link_is_not_one() {
        let text = [
            "[^1]: A note at the foot of the page, not a link to `A`.",
            "<!-- [hidden](missing.md) --> and [shown](shown.md) <!-- [again](missing.md) -->",
            "[two words](not a link.md) and [a title](titled.md 'Title')",
            "A span, ```rust``` and then [after it](after.md).",
        ]
        .join("\n");

        let targets: Vec<String> = links(&text).into_iter().map(|link| link.target).collect();

        assert_eq!(targets, ["shown.md", "titled.md", "after.md"]);
    }

    #[test]
    fn a_path_or_a_fragment_may_be_written_with_escapes_and_a_query() {
        assert_eq!(
            found(&[
                (
                    "docs/page.md",
                    "[a](two%20words.md#a%20b) [b](build-plan.md?plain=1#the-build-plan)\n"
                ),
                ("docs/two words.md", "<a id=\"a b\"></a>\n"),
                ("docs/build-plan.md", PLAN),
            ]),
            [] as [&str; 0]
        );
        assert_one(
            &found(&[("docs/page.md", "[a](two%20words.md)\n")]),
            &["`docs/two words.md`, which is not in the repository"],
        );
    }

    #[test]
    fn a_headings_name_is_githubs() {
        for (heading, name) in [
            ("Where the five minutes go", "where-the-five-minutes-go"),
            (
                "Step 10: editing tools and the plan",
                "step-10-editing-tools-and-the-plan",
            ),
            (
                "After a lockfile change: `rust-cache` alone, or with `sccache`",
                "after-a-lockfile-change-rust-cache-alone-or-with-sccache",
            ),
            ("`cortex_tools::run_blocking`", "cortex_toolsrun_blocking"),
            ("A [link](other.md#x) in a heading", "a-link-in-a-heading"),
            (
                "Tiers 1 & 2 (unit, integration)",
                "tiers-1--2-unit-integration",
            ),
            ("Größe und Maße", "größe-und-maße"),
            ("What's new?", "whats-new"),
        ] {
            assert_eq!(slug(heading), name, "{heading}");
        }
    }

    #[test]
    fn headings_with_the_same_name_are_numbered() {
        let page = "\
# Tools

## Reading

### Limits

## Writing ##

### Limits

Setext
------

### Limits

<a id=\"by-hand\"></a> and <a name=\"older\"></a>
";
        let anchors: Vec<String> = heading_anchors(page).into_iter().collect();

        assert_eq!(
            anchors,
            [
                "by-hand", "limits", "limits-1", "limits-2", "older", "reading", "setext", "tools",
                "writing"
            ]
        );
    }

    #[test]
    fn a_number_that_is_taken_is_passed_over_and_a_quoted_heading_counts() {
        let page = "## Limits\n\n## Limits 1\n\n## Limits\n\n> ## Quoted\n";

        let anchors: Vec<String> = heading_anchors(page).into_iter().collect();

        // The second `Limits` cannot be `limits-1`: a heading has that name.
        assert_eq!(anchors, ["limits", "limits-1", "limits-2", "quoted"]);
    }

    #[test]
    fn a_rule_or_a_table_is_not_a_heading() {
        let page = "Text.\n\n---\n\n| a | b |\n|---|---|\n\n- item\n---\n";

        assert_eq!(heading_anchors(page), BTreeSet::new());
    }

    #[test]
    fn a_docs_path_named_in_code_must_be_there() {
        // Each line of the source begins this file's line with a quote, so
        // that this file's own comments are all the rule reads of it.
        let source = [
            "//! The tiers are in `docs/testing-and-ci.md`, and the rules in docs/rules.md.",
            "//! A step's record is `docs/steps/<NN>/ci.md`; see also docs/architecture/*.md.",
            "const PAGE: &str = \"docs/architecture/01-workspace.md\";",
            "    let x = 1; // A comment after code names docs/gone.md too.",
            "let of_a_test = scratch.file(\"docs/guide.md\", \"A file a test made up.\");",
        ]
        .join("\n");
        let found = found(&[
            ("xtask/src/tiers.rs", source.as_str()),
            ("docs/testing-and-ci.md", "# Testing and CI\n"),
            ("docs/architecture/01-workspace.md", "# The workspace\n"),
        ]);

        // The two in comments. The string on the last line is a test's own
        // file, and is not read.
        assert_eq!(found.len(), 2, "{found:#?}");
        assert!(
            found[0].contains(
                "xtask/src/tiers.rs:1: names `docs/rules.md`, which is not in the repository"
            ),
            "{}",
            found[0]
        );
        assert!(
            found[1].contains("xtask/src/tiers.rs:4: names `docs/gone.md`"),
            "{}",
            found[1]
        );
    }

    #[test]
    fn a_comment_is_what_follows_two_slashes_outside_a_string() {
        use super::comment_of;

        assert_eq!(comment_of("    // whole line"), Some(" whole line"));
        assert_eq!(comment_of("code(); // after code"), Some(" after code"));
        assert_eq!(comment_of("/// a doc comment"), Some("/ a doc comment"));
        assert_eq!(comment_of("let url = \"https://example.org\";"), None);
        assert_eq!(
            comment_of("let url = \"https://example.org\"; // the site"),
            Some(" the site")
        );
        assert_eq!(comment_of("let quoted = \"a \\\" // b\";"), None);
        assert_eq!(comment_of("let x = a / b;"), None);
    }

    #[test]
    fn outside_rust_source_every_line_is_read() {
        let found = found(&[
            (".github/workflows/ci.yml", "run: cat docs/gone.md\n"),
            ("Cargo.toml", "readme = \"docs/gone.md\"\n"),
            ("README.md", "The plan is docs/gone.md.\n"),
        ]);

        assert_eq!(found.len(), 3, "{found:#?}");
    }

    #[test]
    fn docs_paths_are_read_whole_and_only_from_the_root() {
        let text = "\
docs/a.md, (docs/b-c.md). \"docs/d_e/f.md\" docs/g.md#heading
../docs/h.md crates/x/docs/i.md docs/j.txt docs/k docs/<step>/l.md
";
        assert_eq!(
            named_docs(text, false),
            [
                (1, "docs/a.md".to_owned()),
                (1, "docs/b-c.md".to_owned()),
                (1, "docs/d_e/f.md".to_owned()),
                (1, "docs/g.md".to_owned()),
            ]
        );
    }

    #[test]
    fn test_data_may_name_pages_that_never_were() {
        assert_eq!(
            found(&[(
                "xtask/fixtures/links/page.rs",
                "// see docs/never-written.md\n"
            )]),
            [] as [&str; 0]
        );
    }
}
