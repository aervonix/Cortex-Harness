//! The subset of PowerShell the analysis models: **one command with
//! literal arguments** (`docs/architecture/06-permissions-and-sandbox.md`,
//! "Shell command analysis").
//!
//! PowerShell is a programming language whose parser switches between two
//! modes inside one line, and it reads many characters outside ASCII as
//! their ASCII look-alikes. The subset is therefore narrower than the POSIX
//! one, and everything outside it is asked about.
//!
//! # What is modelled
//!
//! - One command: a name, then arguments separated by spaces or tabs.
//! - Words of ASCII letters, digits and `_ - . / \ : = +`.
//! - A whole word in single quotes, where `''` is one quote, or in double
//!   quotes with no `$` and no back-tick, where `""` is one quote.
//! - `~` as a whole word or before a separator: the home directory.
//! - `*` and `?` in an argument. The word is flagged as a pattern.
//!
//! # What is not
//!
//! | Written | Why |
//! |---|---|
//! | `;`, `\|`, `&`, `&&`, `\|\|`, a line break | More than one command, or the call operator. |
//! | `$`, a back-tick | A variable, a sub-expression, an escape. |
//! | `(`, `)`, `{`, `}`, `[`, `]` | A sub-expression, a script block, a type or an index. |
//! | `@`, `,`, `%`, `#`, `!`, `<`, `>` | Splatting and arrays, the `ForEach-Object` alias and the stop-parsing token `--%`, a comment, negation, redirection. |
//! | A quote that is not a whole word | PowerShell splits `"a b"c` in a way that depends on what follows. |
//! | Any character outside ASCII that is not inside quotes | PowerShell reads several dashes as `-` and several spaces as a space. |
//! | A typographic quote, anywhere | PowerShell reads `‘ ’ ‚ ‛` as `'` and `“ ” „` as `"`, inside a quoted text too. |
//! | A quoted command name, or one that is not a plain name or path | It is an expression, not a command. |
//! | A keyword as the command | `if`, `foreach`, `function`, `using` and the rest of [`KEYWORDS`]. |
//! | `powershell`, `pwsh`, `cmd`, `Invoke-Expression`, `iex`, `.` | Another shell, or text run as code: this is where an encoded command would be. |

use super::{Joiner, NotModelled, Part, Word};

/// The words PowerShell reads as part of its language when they stand where
/// a command is expected. Compared without regard to case.
pub const KEYWORDS: [&str; 38] = [
    "begin",
    "break",
    "catch",
    "class",
    "continue",
    "data",
    "define",
    "do",
    "dynamicparam",
    "else",
    "elseif",
    "end",
    "enum",
    "exit",
    "filter",
    "finally",
    "for",
    "foreach",
    "from",
    "function",
    "hidden",
    "if",
    "in",
    "inlinescript",
    "parallel",
    "param",
    "process",
    "return",
    "sequence",
    "static",
    "switch",
    "throw",
    "trap",
    "try",
    "until",
    "using",
    "var",
    "while",
];

/// Commands that start another shell or run text as code. A line with one
/// is not modelled, whatever its arguments: `powershell -EncodedCommand …`
/// is the plainest way to hide a command from a rule.
pub const RUNS_TEXT: [&str; 6] = ["powershell", "pwsh", "cmd", "invoke-expression", "iex", "."];

/// The quote characters PowerShell reads as `'` or `"` besides the ASCII
/// ones.
const TYPOGRAPHIC_QUOTES: [char; 7] = [
    '\u{2018}', '\u{2019}', '\u{201a}', '\u{201b}', '\u{201c}', '\u{201d}', '\u{201e}',
];

fn refuse<T>(why: impl Into<String>) -> Result<T, NotModelled> {
    Err(NotModelled::because(why))
}

fn plain(character: char) -> bool {
    character.is_ascii_alphanumeric()
        || matches!(character, '_' | '-' | '.' | '/' | '\\' | ':' | '=' | '+')
}

fn blank(character: char) -> bool {
    matches!(character, ' ' | '\t')
}

/// Reads a command line. One part, or why the line is not modelled.
pub(crate) fn parts(command: &str) -> Result<Vec<Part>, NotModelled> {
    let mut words: Vec<Word> = Vec::new();
    let mut chars = command.chars().peekable();
    while let Some(&first) = chars.peek() {
        if blank(first) {
            chars.next();
            continue;
        }
        let word = match first {
            '\'' | '"' => {
                chars.next();
                let word = quoted(&mut chars, first)?;
                if chars.peek().is_some_and(|&next| !blank(next)) {
                    return refuse(
                        "a quoted text with more text right after it, which PowerShell may split",
                    );
                }
                word
            }
            _ => bare(&mut chars)?,
        };
        if !word.quoted && word.text.starts_with('-') && word.text.ends_with(':') {
            return refuse(
                "a parameter that ends with `:`, which takes the next word as its value",
            );
        }
        words.push(word);
    }
    let Some(command_word) = words.first() else {
        return refuse("an empty command");
    };
    check_command_word(command_word)?;
    Ok(vec![Part {
        joiner: Joiner::First,
        words,
        redirects: Vec::new(),
    }])
}

/// Reads a quoted word, after its opening quote.
fn quoted(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    quote: char,
) -> Result<Word, NotModelled> {
    let mut text = String::new();
    loop {
        let Some(character) = chars.next() else {
            return refuse("a quote that is not closed");
        };
        if character == quote {
            // A doubled quote is the quote itself.
            if chars.peek() == Some(&quote) {
                chars.next();
                text.push(quote);
                continue;
            }
            break;
        }
        if TYPOGRAPHIC_QUOTES.contains(&character) {
            return refuse("a typographic quote, which PowerShell reads as a plain one");
        }
        if character.is_control() && character != '\t' {
            return refuse("a control character or a line break inside quotes");
        }
        if quote == '"' && matches!(character, '$' | '`') {
            return refuse("`$` or a back-tick inside double quotes: an expansion or an escape");
        }
        text.push(character);
    }
    Ok(Word {
        text,
        quoted: true,
        glob: false,
        home: false,
    })
}

/// Reads a word with no quotes in it.
fn bare(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Result<Word, NotModelled> {
    let mut word = Word::literal("");
    while let Some(&character) = chars.peek() {
        if blank(character) {
            break;
        }
        chars.next();
        match character {
            '*' | '?' => {
                word.glob = true;
                word.text.push(character);
            }
            '~' => {
                let alone = chars
                    .peek()
                    .is_none_or(|&next| blank(next) || matches!(next, '/' | '\\'));
                if !(word.text.is_empty() && alone) {
                    return refuse("a `~` that is not the home directory alone");
                }
                word.home = true;
                word.text.push('~');
            }
            '\'' | '"' => {
                return refuse("a quote inside a word, which PowerShell may split");
            }
            '\n' | '\r' | ';' => return refuse("more than one command on the line"),
            '|' => return refuse("`|`: a pipeline"),
            '&' => return refuse("`&`: the call operator, a background job, or `&&`"),
            '$' => return refuse("`$`: a variable or a sub-expression"),
            '`' => return refuse("a back-tick: PowerShell's escape character"),
            '(' | ')' | '{' | '}' | '[' | ']' => {
                return refuse("a bracket: a sub-expression, a script block, a type or an index");
            }
            '<' | '>' => return refuse("`<` or `>`: a redirection"),
            '@' | ',' => return refuse("`@` or `,`: splatting or an array"),
            '%' => return refuse("`%`: the `ForEach-Object` alias or the stop-parsing token"),
            '#' => return refuse("`#`: a comment"),
            '!' => return refuse("`!`: negation"),
            character if plain(character) => word.text.push(character),
            other => {
                return refuse(format!(
                    "the character {other:?}, which the analysis does not model"
                ));
            }
        }
    }
    Ok(word)
}

/// The name of a command without its directory and without `.exe`, in
/// lower case: how PowerShell, which ignores case, finds it.
#[must_use]
pub fn command_name(written: &str) -> String {
    let name = written
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(written)
        .to_ascii_lowercase();
    name.strip_suffix(".exe").unwrap_or(&name).to_owned()
}

/// Checks the word that stands where the command is.
fn check_command_word(word: &Word) -> Result<(), NotModelled> {
    if word.quoted {
        return refuse("a quoted command name, which PowerShell reads as a text, not a command");
    }
    if word.glob || word.home {
        return refuse("a pattern or `~` where a command is expected");
    }
    let text = word.text.as_str();
    let is_path = text.starts_with("./") || text.starts_with(".\\");
    let starts_as_name = text
        .chars()
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic());
    if text != "." && !is_path && !starts_as_name {
        return refuse(
            "a command that does not begin with a letter: PowerShell reads it as an expression \
             or a parameter",
        );
    }
    if text.contains('=') || text.contains('+') {
        return refuse("an expression where a command is expected");
    }
    let name = command_name(text);
    if KEYWORDS.contains(&name.as_str()) {
        return refuse(format!("`{text}`: a PowerShell keyword"));
    }
    if RUNS_TEXT.contains(&name.as_str()) {
        return refuse(format!(
            "`{text}`: another shell, or a command that runs text as code"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use cortex_base::Rng;
    use cortex_testkit::cases::{self, ensure};

    use super::*;

    fn words(command: &str) -> Vec<String> {
        match parts(command) {
            Ok(parts) => {
                assert_eq!(parts.len(), 1, "one command");
                assert!(parts[0].redirects.is_empty());
                parts[0]
                    .words
                    .iter()
                    .map(|word| {
                        let mut text = word.text.clone();
                        if word.glob {
                            text.push_str("(glob)");
                        }
                        if word.home {
                            text.push_str("(home)");
                        }
                        text
                    })
                    .collect()
            }
            Err(NotModelled(why)) => panic!("{command:?} is not modelled: {why}"),
        }
    }

    #[test]
    fn one_command_with_literal_arguments_is_read() {
        assert_eq!(words("Get-ChildItem"), ["Get-ChildItem"]);
        assert_eq!(
            words("Get-ChildItem -Path src -Recurse"),
            ["Get-ChildItem", "-Path", "src", "-Recurse"]
        );
        assert_eq!(
            words(r"  Get-Content   C:\Work\App\README.md  -TotalCount 5 "),
            ["Get-Content", r"C:\Work\App\README.md", "-TotalCount", "5"]
        );
        assert_eq!(words("git status --short"), ["git", "status", "--short"]);
        assert_eq!(words(r".\build.ps1 -Fast"), [r".\build.ps1", "-Fast"]);
        assert_eq!(
            words(r"C:\Tools\rg.exe -n TODO src"),
            [r"C:\Tools\rg.exe", "-n", "TODO", "src"]
        );
        assert_eq!(
            words("Select-String -Pattern 'a b' -Path \"my file.txt\""),
            ["Select-String", "-Pattern", "a b", "-Path", "my file.txt"]
        );
        // A doubled quote is the quote itself.
        assert_eq!(words("echo 'it''s'"), ["echo", "it's"]);
        assert_eq!(words("echo \"say \"\"hi\"\"\""), ["echo", "say \"hi\""]);
        // Everything but `$` and the back-tick is literal in double quotes,
        // and everything is in single quotes.
        assert_eq!(
            words("echo '$env:PATH `n ; | & ( ) { } @ , % # é'"),
            ["echo", "$env:PATH `n ; | & ( ) { } @ , % # é"]
        );
        assert_eq!(
            words("echo \"; | & ( ) { } @ , % #\""),
            ["echo", "; | & ( ) { } @ , % #"]
        );
        assert_eq!(words("ls *.rs"), ["ls", "*.rs(glob)"]);
        assert_eq!(words(r"ls ~\notes"), ["ls", r"~\notes(home)"]);
        assert_eq!(words("ls ~"), ["ls", "~(home)"]);
        assert_eq!(
            words("Get-Content -Path:README.md"),
            ["Get-Content", "-Path:README.md"]
        );
    }

    /// Everything else is asked about.
    #[test]
    fn anything_else_is_refused() {
        for command in [
            // More than one command.
            "ls; pwd",
            "ls | sort",
            "ls && pwd",
            "ls || pwd",
            "ls\npwd",
            "ls\r\npwd",
            "ls &",
            // The call operator and dot-sourcing.
            "& git status",
            "& 'C:\\Tools\\x.exe'",
            ". .\\profile.ps1",
            // Variables, sub-expressions, escapes.
            "echo $env:PATH",
            "echo $(Get-Date)",
            "echo \"$env:PATH\"",
            "echo \"a`nb\"",
            "echo a`b",
            "echo (Get-Date)",
            "ls -Filter {x}",
            "Where-Object { $_.Name }",
            "[System.IO.File]::ReadAllText('x')",
            "echo [int]",
            "echo a[0]",
            // Splatting, arrays, the stop-parsing token, comments.
            "echo @args",
            "echo @('a')",
            "cargo build --features a,b",
            "cmd2 --% anything",
            "git log --format=%H",
            "ls # comment",
            "echo !x",
            // Redirection.
            "echo hi > out.txt",
            "echo hi 2>&1",
            "sort < in.txt",
            // A quote that is not a whole word.
            "echo a'b'",
            "echo 'a'b",
            "echo \"a b\"c",
            "git commit -m=\"x y\"",
            "Get-Content -Path: README.md",
            "Get-Content -Path:",
            "echo 'unterminated",
            "echo \"unterminated",
            // Characters PowerShell reads as their ASCII look-alikes.
            "echo \u{2018}x\u{2019}",
            "echo 'a\u{2019}; rm x'",
            "echo \"a\u{201d}; rm x\"",
            "powershell \u{2013}EncodedCommand AAAA",
            "ls \u{2014}Recurse",
            "ls\u{a0}-Recurse",
            "ls é",
            "echo 'a\nb'",
            "echo 'a\u{0}'",
            // A command that is not a plain name.
            "'git' status",
            "\"git\" status",
            "-Recurse",
            "1+1",
            "1",
            "*",
            "?",
            "~",
            "x=1",
            "a+b",
            "\\\\server\\share\\x.exe",
            // Keywords.
            "if x",
            "ForEach x in y",
            "function f",
            "using module x",
            "exit 3",
            "return",
            // Other shells and text run as code.
            "powershell -EncodedCommand AAAA",
            "powershell.exe -enc AAAA",
            "PowerShell -e AAAA",
            "pwsh -Command ls",
            r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe -ec AAAA",
            "cmd /c dir",
            "CMD.EXE /c dir",
            "Invoke-Expression 'ls'",
            "iex 'ls'",
            "IEX x",
            // Nothing.
            "",
            "   ",
            "\t",
            // `~` forms that are not the home directory.
            "ls ~user",
            "ls a~b",
        ] {
            assert!(
                parts(command).is_err(),
                "{command:?} was modelled as {:?}",
                parts(command)
            );
        }
    }

    #[test]
    fn a_command_is_named_without_its_directory_its_extension_or_its_case() {
        for (written, name) in [
            ("git", "git"),
            ("Git.EXE", "git"),
            (r"C:\Program Files\Git\cmd\git.exe", "git"),
            ("/usr/bin/git", "git"),
            ("Get-ChildItem", "get-childitem"),
            (r".\build.ps1", "build.ps1"),
        ] {
            assert_eq!(command_name(written), name);
        }
    }

    const PIECES: [&str; 30] = [
        "ls",
        "Get-Content",
        "git",
        "-Path",
        "src",
        " ",
        " ",
        " ",
        "'q'",
        "\"d\"",
        "*",
        "~",
        ";",
        "|",
        "&",
        "$",
        "`",
        "(",
        ")",
        "{",
        "}",
        "[",
        "@",
        ",",
        "%",
        ">",
        "\n",
        "'",
        "\"",
        "x",
    ];

    /// A modelled line is one command, and outside its quotes it holds none
    /// of the characters that make PowerShell do more than pass a word on.
    #[test]
    fn a_modelled_line_is_one_command_of_inert_words() {
        cases::check(
            "powershell_modelled_lines_are_inert",
            4000,
            |rng| {
                (0..1 + cases::small(rng, 8))
                    .map(|_| *cases::pick(rng, &PIECES))
                    .collect::<String>()
            },
            |line| {
                let Ok(parts) = parts(line) else {
                    return Ok(());
                };
                ensure!(parts.len() == 1, "more than one command");
                ensure!(!parts[0].words.is_empty(), "no command");
                let mut quote: Option<char> = None;
                let mut chars = line.chars().peekable();
                while let Some(character) = chars.next() {
                    match quote {
                        Some(open) if character == open => {
                            if chars.peek() == Some(&open) {
                                chars.next();
                            } else {
                                quote = None;
                            }
                        }
                        Some('"') => ensure!(
                            !matches!(character, '$' | '`'),
                            "{character:?} inside double quotes"
                        ),
                        Some(_) => {}
                        None if matches!(character, '\'' | '"') => quote = Some(character),
                        None => ensure!(
                            !matches!(
                                character,
                                ';' | '|'
                                    | '&'
                                    | '$'
                                    | '`'
                                    | '('
                                    | ')'
                                    | '{'
                                    | '}'
                                    | '['
                                    | '@'
                                    | ','
                                    | '%'
                                    | '>'
                                    | '\n'
                            ),
                            "{character:?} outside quotes"
                        ),
                    }
                }
                ensure!(quote.is_none(), "a quote left open");
                Ok(())
            },
        );
    }

    #[test]
    fn any_text_is_either_modelled_or_refused() {
        cases::check(
            "powershell_any_text",
            3000,
            |rng| {
                (0..cases::small(rng, 24))
                    .map(|_| match rng.below(3) {
                        0 => char::from_u32(rng.below(0x80) as u32).unwrap_or(' '),
                        1 => *cases::pick(rng, &['é', '\u{a0}', '\u{2019}', '“', '–', '𝒳']),
                        _ => *cases::pick(rng, &['a', ' ', '\'', '"', '-', '~', '*', '\\']),
                    })
                    .collect::<String>()
            },
            |line| {
                let _ = parts(line);
                Ok(())
            },
        );
    }
}
