//! The subset of the POSIX shell language the analysis models.
//!
//! The same line means the same thing in bash, dash, zsh and ksh as long as
//! it stays inside this subset, which is why it is this small.
//!
//! # What is modelled
//!
//! - **Simple commands** joined by `|`, `&&`, `||`, `;` and line breaks.
//! - **Words** made of letters, digits and `_ - . / : = + , @ %`, and
//!   letters and digits outside ASCII.
//! - **Single quotes.** Everything inside is literal.
//! - **Double quotes** with no `$` and no back-tick inside. `\"`, `\\`,
//!   `\$` and `` \` `` are the character itself; any other `\x` is both
//!   characters, as the shell has it.
//! - **`~`** as a whole word or before a `/`: the home directory. Inside
//!   a word it is itself (`HEAD~1`).
//! - **Globs** (`*`, `?`, `[`, `]`). The word is kept and flagged
//!   ([`Word::glob`]): a rule may match it as text, and the built-in table
//!   never treats it as safe, because the names it expands to can begin
//!   with a `-`.
//! - **Redirections** `< file`, `> file`, `>> file`, each with an optional
//!   descriptor number, to a word with no glob; and `2>&1`, `>&2`, `>&-`.
//!
//! # What is not, and is therefore asked about
//!
//! | Written | Why |
//! |---|---|
//! | `$`, `` ` `` | An expansion or a command substitution: the words are not the words written. |
//! | `(`, `)` | A subshell, a function definition, process substitution, an array. |
//! | `{`, `}` anywhere in a word | Brace expansion (`{a,b}`, `{1..3}`) or a group. |
//! | `\` outside quotes | An escape or a line continuation. |
//! | `&`, `\|&`, `&>` | A background job, or both streams at once. |
//! | `<<`, `<<<`, `<>`, `>\|`, `<&`, `<(`, `>(` | A here-document and its relations. |
//! | `#`, `!`, `^` | A comment, history or negation, and zsh's glob operators. |
//! | A word that begins with `=` | zsh replaces `=name` with the path of the program `name`. |
//! | `~user`, `~+`, `~-`, a `~` after `=` or `:` | Another directory than the one written, or one that depends on the shell. |
//! | `20>file`: several digits before a redirection | A descriptor number to bash, an argument to dash. |
//! | `NAME=value` where a command is expected | A variable assignment: it can change what the next command means. |
//! | A shell keyword or a state-changing builtin as the command | `if`, `for`, `time`, `eval`, `exec`, `source`, `.`, `export`, `alias`, `set`, `trap` and the rest of [`NOT_COMMANDS`]. |
//! | A glob in the command's own name, or an empty name | `[[ … ]]`, `[ … ]`, and nothing a table or a rule could name. |
//! | A quote left open, an operator with nothing beside it, `;;` | It does not parse. |
//! | Any other character | Not on the list. |
//!
//! Control characters are refused everywhere except a tab or a line break,
//! and those two are refused inside the home-directory and descriptor
//! forms where they could end a word early.

use super::{Joiner, NotModelled, Part, Redirect, Word};

/// Words that are not the name of a program when they stand where a command
/// is expected: the shell's keywords, and the builtins that run text as
/// code or change what later commands mean. A line with one is not
/// modelled.
///
/// `cd` is not here: it is modelled, because the directory a later part
/// runs in can be followed (`crate::engine`).
pub const NOT_COMMANDS: [&str; 51] = [
    // Keywords.
    "if", "then", "else", "elif", "fi", "for", "while", "until", "do", "done", "case", "esac",
    "function", "select", "time", "coproc", "in",
    // Builtins that run their arguments, or another file, as code.
    "eval", "exec", "source", ".", "trap", "command", "builtin",
    // Builtins that change what a later command means or sees.
    "alias", "unalias", "set", "unset", "shopt", "setopt", "unsetopt", "export", "declare",
    "typeset", "local", "readonly", "enable", "hash", "umask", "ulimit", "pushd", "popd",
    // Builtins that manage jobs or end the shell early.
    "exit", "return", "break", "continue", "wait", "fg", "bg", "disown", "read",
];

/// Whether a character may stand in a word without quotes.
fn plain(character: char) -> bool {
    character.is_ascii_alphanumeric()
        || matches!(
            character,
            '_' | '-' | '.' | '/' | ':' | '=' | '+' | ',' | '@' | '%'
        )
        || (!character.is_ascii() && character.is_alphanumeric())
}

/// Whether a character ends a word.
fn ends_word(character: char) -> bool {
    matches!(character, ' ' | '\t' | '\n' | ';' | '|' | '&' | '<' | '>')
}

/// What the word being read will be used as.
enum Pending {
    /// An ordinary word of the command.
    Argument,
    /// The file of a `<`.
    ReadTarget,
    /// The file of a `>` or `>>`.
    WriteTarget { append: bool },
}

struct Reader<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
    done: Vec<Part>,
    current: Part,
    word: Option<Word>,
    pending: Pending,
}

fn refuse<T>(why: impl Into<String>) -> Result<T, NotModelled> {
    Err(NotModelled::because(why))
}

/// Reads a command line. The parts, or why the line is not modelled.
pub(crate) fn parts(command: &str) -> Result<Vec<Part>, NotModelled> {
    let mut reader = Reader {
        chars: command.chars().peekable(),
        done: Vec::new(),
        current: Part {
            joiner: Joiner::First,
            words: Vec::new(),
            redirects: Vec::new(),
        },
        word: None,
        pending: Pending::Argument,
    };
    while let Some(character) = reader.chars.next() {
        reader.step(character)?;
    }
    reader.end_word()?;
    reader.end_part(Joiner::Then, true)?;
    if reader.done.is_empty() {
        return refuse("an empty command");
    }
    Ok(reader.done)
}

impl Reader<'_> {
    fn word(&mut self) -> &mut Word {
        self.word.get_or_insert_with(|| Word::literal(""))
    }

    fn step(&mut self, character: char) -> Result<(), NotModelled> {
        match character {
            ' ' | '\t' => self.end_word(),
            '\n' | ';' => {
                self.end_word()?;
                if character == ';' && matches!(self.chars.peek(), Some(';' | '&')) {
                    return refuse("`;;` or `;&`, which belong to a `case`");
                }
                // A line break right after `|`, `&&` or `||` continues the
                // list on the next line.
                if character == '\n'
                    && self.current.words.is_empty()
                    && self.current.redirects.is_empty()
                    && matches!(self.pending, Pending::Argument)
                    && matches!(self.current.joiner, Joiner::Pipe | Joiner::And | Joiner::Or)
                {
                    return Ok(());
                }
                self.end_part(Joiner::Then, character == '\n')
            }
            '|' => {
                self.end_word()?;
                let joiner = match self.chars.peek() {
                    Some('|') => {
                        self.chars.next();
                        Joiner::Or
                    }
                    Some('&') => return refuse("`|&`, which pipes both output streams"),
                    _ => Joiner::Pipe,
                };
                self.end_part(joiner, false)
            }
            '&' => {
                self.end_word()?;
                if self.chars.peek() == Some(&'&') {
                    self.chars.next();
                    self.end_part(Joiner::And, false)
                } else {
                    refuse("`&`: a background job, or a redirection of both output streams")
                }
            }
            '<' | '>' => self.redirection(character),
            '\'' => self.single_quoted(),
            '"' => self.double_quoted(),
            '*' | '?' | '[' | ']' => {
                let word = self.word();
                word.glob = true;
                word.text.push(character);
                Ok(())
            }
            '~' => {
                let Some(before) = self.word.as_ref().map(|word| word.text.chars().next_back())
                else {
                    // At the start of a word: the home directory, and only
                    // that.
                    let alone = self
                        .chars
                        .peek()
                        .is_none_or(|&next| next == '/' || ends_word(next));
                    if !alone {
                        return refuse(
                            "a `~` that is not the home directory alone: `~user`, `~+`, `~-`",
                        );
                    }
                    let word = self.word();
                    word.home = true;
                    word.text.push('~');
                    return Ok(());
                };
                // Inside a word a `~` is itself (`HEAD~1`), except right
                // after `=` or `:`, where bash expands it in a word that
                // reads like an assignment (`a=~/x`, `PATH=a:~/b`).
                if matches!(before, Some('=' | ':') | None) {
                    return refuse(
                        "a `~` after `=`, `:` or a quote, which some shells expand to the home \
                         directory",
                    );
                }
                self.word().text.push('~');
                Ok(())
            }
            '=' if self.word.is_none() => {
                refuse("a word that begins with `=`, which zsh replaces with a program's path")
            }
            '$' => refuse("`$`: a variable or another expansion"),
            '`' => refuse("a back-tick: command substitution"),
            '(' | ')' => {
                refuse("`(` or `)`: a subshell, a function, process substitution or an array")
            }
            '{' | '}' => refuse("`{` or `}`: brace expansion or a group of commands"),
            '\\' => refuse("a backslash outside quotes: an escape or a line continuation"),
            '#' => refuse("`#`: a comment"),
            '!' => refuse("`!`: history expansion or negation"),
            character if plain(character) => {
                self.word().text.push(character);
                Ok(())
            }
            other => refuse(format!(
                "the character {other:?}, which the analysis does not model"
            )),
        }
    }

    fn single_quoted(&mut self) -> Result<(), NotModelled> {
        self.word().quoted = true;
        loop {
            match self.chars.next() {
                None => return refuse("a single quote that is not closed"),
                Some('\'') => return Ok(()),
                Some(inner) => self.quoted_char(inner)?,
            }
        }
    }

    fn double_quoted(&mut self) -> Result<(), NotModelled> {
        self.word().quoted = true;
        loop {
            match self.chars.next() {
                None => return refuse("a double quote that is not closed"),
                Some('"') => return Ok(()),
                Some('$') => {
                    return refuse("`$` inside double quotes: a variable or another expansion");
                }
                Some('`') => {
                    return refuse("a back-tick inside double quotes: command substitution");
                }
                Some('\\') => match self.chars.next() {
                    None => return refuse("a double quote that is not closed"),
                    Some('\n') => return refuse("a line continuation inside double quotes"),
                    Some(escaped @ ('"' | '\\' | '$' | '`')) => self.quoted_char(escaped)?,
                    Some(other) => {
                        // The shell keeps the backslash before any other
                        // character.
                        self.quoted_char('\\')?;
                        self.quoted_char(other)?;
                    }
                },
                Some(inner) => self.quoted_char(inner)?,
            }
        }
    }

    /// Adds a character from inside quotes. Everything is literal there
    /// except a control character, which no argument a person approves
    /// should hide.
    fn quoted_char(&mut self, character: char) -> Result<(), NotModelled> {
        if character.is_control() && !matches!(character, '\n' | '\t') {
            return refuse("a control character inside quotes");
        }
        self.word().text.push(character);
        Ok(())
    }

    fn redirection(&mut self, operator: char) -> Result<(), NotModelled> {
        // A digit right before the operator is a descriptor number, not an
        // argument: `2>file`. More digits are a descriptor to bash and an
        // argument to dash, so they are not modelled.
        if let Some(word) = &self.word
            && !word.quoted
            && !word.glob
            && !word.home
            && !word.text.is_empty()
            && word.text.bytes().all(|byte| byte.is_ascii_digit())
        {
            if word.text.len() > 1 {
                return refuse(
                    "a number of several digits right before a redirection, which bash reads as \
                     a descriptor and other shells as an argument",
                );
            }
            self.word = None;
        }
        self.end_word()?;
        if !matches!(self.pending, Pending::Argument) {
            return refuse("a redirection with no file");
        }
        if operator == '<' {
            return match self.chars.peek() {
                Some('<') => refuse("`<<` or `<<<`: a here-document or a here-string"),
                Some('>') => refuse("`<>`, which opens a file for reading and writing"),
                Some('(') => refuse("`<(`: process substitution"),
                Some('&') => refuse("`<&`, which copies an input descriptor"),
                _ => {
                    self.pending = Pending::ReadTarget;
                    Ok(())
                }
            };
        }
        let mut append = false;
        match self.chars.peek() {
            Some('>') => {
                self.chars.next();
                append = true;
            }
            Some('|') => return refuse("`>|`, which overwrites despite `noclobber`"),
            Some('(') => return refuse("`>(`: process substitution"),
            _ => {}
        }
        if self.chars.peek() != Some(&'&') {
            self.pending = Pending::WriteTarget { append };
            return Ok(());
        }
        self.chars.next();
        // `>&2`, `2>&1`, `>&-`: one of the command's own descriptors again,
        // or one closed. `>&file` sends both streams to a file in bash.
        let mut target = String::new();
        while let Some(&next) = self.chars.peek() {
            if ends_word(next) {
                break;
            }
            target.push(next);
            self.chars.next();
        }
        let is_descriptor = target == "-"
            || (!target.is_empty() && target.bytes().all(|byte| byte.is_ascii_digit()));
        if append || !is_descriptor {
            return refuse("`>&` to something that is not a descriptor number");
        }
        self.current.redirects.push(Redirect::Duplicate);
        Ok(())
    }

    fn end_word(&mut self) -> Result<(), NotModelled> {
        let Some(word) = self.word.take() else {
            return Ok(());
        };
        match std::mem::replace(&mut self.pending, Pending::Argument) {
            Pending::Argument => {
                if self.current.words.is_empty() {
                    check_command_word(&word)?;
                }
                self.current.words.push(word);
            }
            Pending::ReadTarget => {
                if word.glob {
                    return refuse("a redirection from a pattern");
                }
                self.current.redirects.push(Redirect::Read(word));
            }
            Pending::WriteTarget { append } => {
                if word.glob {
                    return refuse("a redirection to a pattern");
                }
                self.current.redirects.push(Redirect::Write {
                    target: word,
                    append,
                });
            }
        }
        Ok(())
    }

    /// Ends the part being read. `may_be_empty` is for a separator that may
    /// stand after nothing: a line break, or the end of the line.
    fn end_part(&mut self, next: Joiner, may_be_empty: bool) -> Result<(), NotModelled> {
        if !matches!(self.pending, Pending::Argument) {
            return refuse("a redirection with no file");
        }
        if self.current.words.is_empty() {
            if !self.current.redirects.is_empty() {
                return refuse("a redirection with no command");
            }
            if may_be_empty && matches!(self.current.joiner, Joiner::First | Joiner::Then) {
                // An empty line, or a line that ends with `;`.
                self.current.joiner = if self.done.is_empty() {
                    Joiner::First
                } else {
                    Joiner::Then
                };
                return Ok(());
            }
            return refuse("an operator with no command beside it");
        }
        let finished = std::mem::replace(
            &mut self.current,
            Part {
                joiner: next,
                words: Vec::new(),
                redirects: Vec::new(),
            },
        );
        self.done.push(finished);
        Ok(())
    }
}

/// Checks the word that stands where a command is expected.
fn check_command_word(word: &Word) -> Result<(), NotModelled> {
    if word.text.is_empty() {
        return refuse("an empty command name");
    }
    if word.glob {
        return refuse("a pattern where a command is expected, such as `[` or `[[`");
    }
    if word.home {
        return refuse("a command named by `~`");
    }
    // Quoted or not: a name with `=` in it is never a program a rule or a
    // table would name, and refusing both needs no model of when the shell
    // takes one for an assignment.
    if word.text.contains('=') {
        return refuse("a variable assignment before a command");
    }
    if NOT_COMMANDS.contains(&word.text.as_str()) {
        return refuse(format!(
            "`{}`: a shell keyword, or a builtin that runs text as code or changes what later \
             commands mean",
            word.text
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use cortex_base::Rng;
    use cortex_testkit::cases::{self, ensure};

    use super::*;

    /// A part in a short form: the joiner, the words (with `*` after a
    /// word that is a glob and `~` after one for the home directory), and
    /// the redirections.
    fn show(parts: &[Part]) -> Vec<String> {
        parts
            .iter()
            .map(|part| {
                let mut text = format!("{:?}:", part.joiner);
                for word in &part.words {
                    text.push_str(&format!(" [{}]", word.text));
                    if word.glob {
                        text.push_str("(glob)");
                    }
                    if word.home {
                        text.push_str("(home)");
                    }
                }
                for redirect in &part.redirects {
                    match redirect {
                        Redirect::Read(word) => text.push_str(&format!(" <[{}]", word.text)),
                        Redirect::Write { target, append } => {
                            let arrow = if *append { ">>" } else { ">" };
                            text.push_str(&format!(" {arrow}[{}]", target.text));
                        }
                        Redirect::Duplicate => text.push_str(" dup"),
                    }
                }
                text
            })
            .collect()
    }

    fn modelled(command: &str) -> Vec<String> {
        match parts(command) {
            Ok(parts) => show(&parts),
            Err(NotModelled(why)) => panic!("{command:?} is not modelled: {why}"),
        }
    }

    #[test]
    fn simple_commands_and_their_joiners_are_read() {
        assert_eq!(modelled("ls -la src"), ["First: [ls] [-la] [src]"]);
        assert_eq!(
            modelled("cargo test && git status"),
            ["First: [cargo] [test]", "And: [git] [status]"]
        );
        assert_eq!(
            modelled("a | b || c ; d\ne"),
            [
                "First: [a]",
                "Pipe: [b]",
                "Or: [c]",
                "Then: [d]",
                "Then: [e]"
            ]
        );
        // Blank lines, a trailing `;` and spaces around operators.
        assert_eq!(
            modelled("\n\n  ls ;\n\n pwd  \n"),
            ["First: [ls]", "Then: [pwd]"]
        );
        assert_eq!(modelled("a&&b|c"), ["First: [a]", "And: [b]", "Pipe: [c]"]);
        // A list goes on after a line break that follows its operator.
        assert_eq!(
            modelled("cargo build &&\n  cargo test |\n\n tail -3"),
            [
                "First: [cargo] [build]",
                "And: [cargo] [test]",
                "Pipe: [tail] [-3]"
            ]
        );
        assert_eq!(
            modelled("grep -rn --include=*.rs foo src/"),
            ["First: [grep] [-rn] [--include=*.rs](glob) [foo] [src/]"]
        );
        assert_eq!(modelled("cat résumé.txt"), ["First: [cat] [résumé.txt]"]);
    }

    #[test]
    fn quotes_are_taken_off_and_what_is_inside_is_literal() {
        assert_eq!(
            modelled("echo 'a b' \"c d\""),
            ["First: [echo] [a b] [c d]"]
        );
        assert_eq!(modelled("echo ''"), ["First: [echo] []"]);
        assert_eq!(modelled("echo a'b'\"c\"d"), ["First: [echo] [abcd]"]);
        // Everything is literal in single quotes, the characters that are
        // refused outside them included.
        assert_eq!(
            modelled("echo '$HOME `id` $(id) {a,b} ; | & < > # ! \\ ~ * ='"),
            ["First: [echo] [$HOME `id` $(id) {a,b} ; | & < > # ! \\ ~ * =]"]
        );
        // In double quotes too, except `$` and the back-tick.
        assert_eq!(
            modelled("echo \"{a,b} ; | & < > # ! ~ * = ( )\""),
            ["First: [echo] [{a,b} ; | & < > # ! ~ * = ( )]"]
        );
        // The four escapes of double quotes, and a backslash that is kept.
        assert_eq!(
            modelled(r#"echo "a\"b\\c\$d\`e\nf""#),
            [r#"First: [echo] [a"b\c$d`e\nf]"#]
        );
        // A quoted glob or `~` is not a glob or the home directory.
        let quoted = parts("ls '*.rs' \"~/x\"").unwrap();
        assert!(quoted[0].words[1].quoted && !quoted[0].words[1].glob);
        assert!(!quoted[0].words[2].home);
        // A line break inside quotes is part of the word.
        assert_eq!(modelled("echo 'a\nb'"), ["First: [echo] [a\nb]"]);
    }

    #[test]
    fn globs_and_the_home_directory_are_flagged() {
        assert_eq!(modelled("ls *.rs"), ["First: [ls] [*.rs](glob)"]);
        assert_eq!(
            modelled("ls src/[a-c]?.rs"),
            ["First: [ls] [src/[a-c]?.rs](glob)"]
        );
        assert_eq!(modelled("ls ~"), ["First: [ls] [~](home)"]);
        assert_eq!(modelled("ls ~/notes"), ["First: [ls] [~/notes](home)"]);
        assert_eq!(
            modelled("ls ~;pwd"),
            ["First: [ls] [~](home)", "Then: [pwd]"]
        );
        // Inside a word a `~` is itself.
        assert_eq!(
            modelled("git diff HEAD~1 a~ b~/c"),
            ["First: [git] [diff] [HEAD~1] [a~] [b~/c]"]
        );
    }

    #[test]
    fn redirections_are_read_with_their_files() {
        assert_eq!(
            modelled("echo hi > out.txt"),
            ["First: [echo] [hi] >[out.txt]"]
        );
        assert_eq!(modelled("echo hi>>log"), ["First: [echo] [hi] >>[log]"]);
        assert_eq!(modelled("sort < in.txt"), ["First: [sort] <[in.txt]"]);
        assert_eq!(modelled("make 2>&1"), ["First: [make] dup"]);
        assert_eq!(modelled("make >&2"), ["First: [make] dup"]);
        assert_eq!(modelled("make 2>&-"), ["First: [make] dup"]);
        assert_eq!(
            modelled("make 2>/dev/null | tail -5"),
            ["First: [make] >[/dev/null]", "Pipe: [tail] [-5]"]
        );
        assert_eq!(
            modelled("make > out.log 2> err.log"),
            ["First: [make] >[out.log] >[err.log]"]
        );
        // A number with a space before the operator is an argument.
        assert_eq!(modelled("echo 2 > f"), ["First: [echo] [2] >[f]"]);
        assert_eq!(modelled("echo 2>f"), ["First: [echo] >[f]"]);
        // A quoted number is an argument too.
        assert_eq!(modelled("echo '2'>f"), ["First: [echo] [2] >[f]"]);
        assert_eq!(modelled("cat > '~/x y'"), ["First: [cat] >[~/x y]"]);
        assert_eq!(modelled("cat > ~/x"), ["First: [cat] >[~/x]"]);
    }

    /// Every construct the page lists as "ask": none is modelled.
    #[test]
    fn what_the_analysis_does_not_model_is_refused() {
        for command in [
            // Expansions and substitutions.
            "echo $HOME",
            "echo ${HOME}",
            "cat $(id)",
            "cat `id`",
            "echo \"$(id)\"",
            "echo \"`id`\"",
            "echo \"$HOME\"",
            "echo $((1+1))",
            "echo $'a\\nb'",
            "echo $\"x\"",
            "echo ${x:-$(id)}",
            // Process substitution, subshells, functions, groups, arrays.
            "cat <(ls)",
            "tee >(cat)",
            "(ls)",
            "ls; (cd /; ls)",
            "f() { ls; }; f",
            "{ ls; }",
            "a=(1 2)",
            // Brace expansion, anywhere in a word.
            "echo {a,b}",
            "echo {1..3}",
            "cat x{-n,y}",
            "echo a{b",
            "echo a}b",
            "find . -exec rm {} +",
            // A backslash outside quotes.
            "ls foo\\ bar",
            "ls \\\n-la",
            "find . -name \\*.rs",
            // Background jobs and both-stream forms.
            "ls &",
            "ls & pwd",
            "ls |& cat",
            "echo hi &> out",
            "echo hi >& out",
            "echo hi &>> out",
            // Here-documents and their relations.
            "cat <<EOF\nx\nEOF",
            "cat <<< hi",
            "cat <> f",
            "ls >| out",
            "cat <&3",
            "echo hi >>&2",
            // Comments, history, negation, zsh's operators.
            "ls # comment",
            "ls a#b",
            "! ls",
            "ls !$",
            "ls a^b",
            "ls =cat",
            "=ls",
            // Other directories than the one written.
            "ls ~root/x",
            "ls ~+",
            "ls ~-",
            "ls ~~",
            "ls a=~",
            "ls a=~/x",
            "ls PATH=a:~/b",
            "ls --dir=~/x",
            "ls ''~",
            // A number that is a descriptor to one shell and an argument to
            // another.
            "echo 20>f",
            "echo 10<f",
            "echo 123>>f",
            "echo 10>&2",
            "~/bin/tool",
            // Assignments.
            "A=1 ls",
            "A=1",
            "PATH=/tmp:$PATH ls",
            "'A'=1 ls",
            "a[0]=1",
            // Keywords and builtins that change what follows.
            "if true; then ls; fi",
            "for f in a b; do ls; done",
            "while true; do ls; done",
            "case x in x) ls;; esac",
            "time ls",
            "coproc ls",
            "eval ls",
            "exec ls",
            "source x.sh",
            ". x.sh",
            "trap ls EXIT",
            "command ls",
            "builtin cd /",
            "alias ls=rm",
            "export PATH=/tmp",
            "set -e",
            "shopt -s expand_aliases",
            "unset PATH",
            "pushd /tmp",
            "ls; exit 3",
            "umask 000",
            // A pattern or nothing where a command is expected.
            "[[ -f a ]] && ls",
            "[ -f a ] && ls",
            "*",
            "'' ls",
            "\"\"",
            // Lines that do not parse.
            "echo \"unterminated",
            "echo 'unterminated",
            "ls ;; ls",
            "ls ;& ls",
            "ls &&",
            "&& ls",
            "ls | | cat",
            "ls ||",
            "| ls",
            "ls ; ; pwd",
            "ls >",
            "ls > ; pwd",
            "> out",
            "ls > *.txt",
            "cat < *.txt",
            "ls > > f",
            "",
            "   ",
            "\n",
            ";",
            // Characters not on the list.
            "ls\u{0}",
            "ls \u{7}",
            "ls\rpwd",
            "echo 'a\u{1b}[0m'",
            "echo \"a\u{8}\"",
            "ls\u{a0}-la",
            "ls \u{2028} pwd",
            "ls – x",
            "echo “x”",
        ] {
            assert!(
                parts(command).is_err(),
                "{command:?} was modelled as {:?}",
                parts(command).map(|parts| show(&parts))
            );
        }
    }

    #[test]
    fn a_refusal_says_what_it_met() {
        for (command, expected) in [
            ("echo $HOME", "`$`"),
            ("cat `id`", "back-tick"),
            ("ls &", "background job"),
            ("cat <<EOF", "here-document"),
            ("echo {a,b}", "brace expansion"),
            ("A=1 ls", "variable assignment"),
            ("eval ls", "`eval`"),
            ("ls foo\\ bar", "backslash"),
            ("echo 'x", "not closed"),
            ("ls # x", "comment"),
            ("ls =cat", "zsh"),
            ("ls ~root", "`~`"),
        ] {
            let NotModelled(why) = parts(command).unwrap_err();
            assert!(why.contains(expected), "{command:?}: {why}");
        }
    }

    /// The same word is a command in one place and an argument in another.
    #[test]
    fn a_keyword_is_only_refused_where_a_command_is_expected() {
        assert_eq!(
            modelled("echo if then eval"),
            ["First: [echo] [if] [then] [eval]"]
        );
        assert_eq!(modelled("git stash pop"), ["First: [git] [stash] [pop]"]);
        assert_eq!(modelled("echo a=b"), ["First: [echo] [a=b]"]);
        assert!(parts("ls | eval x").is_err());
        assert!(parts("ls && time ls").is_err());
        assert!(parts("ls\nexport A=1").is_err());
    }

    const PIECES: [&str; 38] = [
        "ls", "cat", "echo", "x", "-la", "src/a.rs", "a b", "'q'", "\"d\"", " ", " ", " ", "|",
        "||", "&&", ";", "\n", ">", ">>", "<", "2>&1", "*", "~", "~/x", "$", "`", "(", ")", "{",
        "}", "\\", "&", "#", "!", "=", "'", "\"", "A=1",
    ];

    /// Whatever the line, a modelled line has only words made of what was
    /// written: nothing the tokenizer accepts holds an unquoted character
    /// that the shell would expand, and a part always has a command.
    #[test]
    fn a_modelled_line_never_holds_what_the_shell_would_expand() {
        cases::check(
            "posix_modelled_lines_are_inert",
            4000,
            |rng| {
                (0..1 + cases::small(rng, 10))
                    .map(|_| *cases::pick(rng, &PIECES))
                    .collect::<String>()
            },
            |line| {
                let Ok(parts) = parts(line) else {
                    return Ok(());
                };
                ensure!(!parts.is_empty(), "no parts");
                for part in &parts {
                    ensure!(!part.words.is_empty(), "a part with no command");
                    let command = &part.words[0];
                    ensure!(!command.glob && !command.home, "a command that expands");
                    ensure!(!command.text.contains('='), "an assignment as a command");
                    ensure!(
                        !NOT_COMMANDS.contains(&command.text.as_str()),
                        "`{}` as a command",
                        command.text
                    );
                }
                // Outside quotes, none of these may appear in a modelled
                // line at all.
                let mut quote: Option<char> = None;
                let mut chars = line.chars();
                while let Some(character) = chars.next() {
                    match quote {
                        Some('"') if character == '\\' => {
                            // An escape: the next character is taken as it is.
                            chars.next();
                        }
                        Some(open) if character == open => quote = None,
                        Some('"') => {
                            ensure!(
                                !matches!(character, '$' | '`'),
                                "{character:?} inside double quotes"
                            );
                        }
                        Some(_) => {}
                        None if matches!(character, '\'' | '"') => quote = Some(character),
                        None => ensure!(
                            !matches!(
                                character,
                                '$' | '`' | '(' | ')' | '{' | '}' | '\\' | '#' | '!'
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

    /// No input makes the tokenizer panic or loop.
    #[test]
    fn any_text_is_either_modelled_or_refused() {
        cases::check(
            "posix_any_text",
            3000,
            |rng| {
                (0..cases::small(rng, 24))
                    .map(|_| match rng.below(4) {
                        0 => char::from_u32(rng.below(0x80) as u32).unwrap_or(' '),
                        1 => *cases::pick(rng, &['é', '\u{a0}', '\u{2028}', '“', '–', '𝒳']),
                        _ => *cases::pick(
                            rng,
                            &[
                                'a', ' ', '\'', '"', '|', '&', ';', '>', '<', '~', '*', '2', '-',
                                '\n',
                            ],
                        ),
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
