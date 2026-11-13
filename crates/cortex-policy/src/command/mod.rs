//! Shell command analysis that fails closed
//! (`docs/architecture/06-permissions-and-sandbox.md`, "Shell command
//! analysis").
//!
//! A permission rule on a command is useful only if a command cannot be
//! made to look like another. So the analysis does not parse a shell's
//! grammar and then look for what is dangerous in the tree. It accepts a
//! **small listed subset**, character by character, and everything else is
//! [`Analysis::NotModelled`], which no allow rule and no safe list ever
//! applies to. A construct nobody thought of is refused by construction.
//!
//! | Module | What it does |
//! |---|---|
//! | [`posix`] | The subset of the POSIX shell language: the same in bash, dash, zsh and ksh. |
//! | [`powershell`] | The subset of PowerShell: one command with literal arguments. |
//! | [`pattern`] | The patterns of `exec` rules, and how a command is matched against them. |
//! | [`table`] | The built-in table of commands that only read. |
//!
//! `cmd.exe` is not analysed at all, and neither is a shell this crate
//! does not know (`fish`, `nu`): their commands are never modelled.
//!
//! **The analysis never runs anything.** It never calls a shell, an
//! interpreter or a program found in the workspace. It looks at text.

pub mod pattern;
pub mod posix;
pub mod powershell;
pub mod table;

/// Which language a command line is written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShellSyntax {
    /// A POSIX shell: bash, sh, dash, zsh, ksh.
    Posix,
    /// PowerShell 7 or Windows PowerShell.
    PowerShell,
    /// `cmd.exe`. Not analysed.
    Cmd,
    /// A shell this crate does not know. Not analysed.
    Unknown,
}

impl ShellSyntax {
    /// The syntax of the shell a program is, by the program's name: a path
    /// is cut to its last name, and `.exe` and case are ignored.
    ///
    /// A name that is not listed is [`Unknown`](Self::Unknown), and its
    /// commands are not analysed. The shell tool starts any unknown program
    /// as if it read `-c <command>` like a POSIX shell; that is no reason
    /// to believe it reads the command like one. `fish` reads a backslash
    /// inside single quotes as an escape, so a line that is two harmless
    /// quoted words to bash is something else to fish.
    #[must_use]
    pub fn of_program(program: &str) -> Self {
        let name = program
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(program)
            .to_ascii_lowercase();
        let name = name.strip_suffix(".exe").unwrap_or(&name);
        match name {
            "bash" | "sh" | "dash" | "zsh" | "ksh" | "mksh" | "ash" => Self::Posix,
            "pwsh" | "powershell" => Self::PowerShell,
            "cmd" => Self::Cmd,
            _ => Self::Unknown,
        }
    }
}

/// One word of a command, after quotes are taken off.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Word {
    /// The word's text. A glob character or a leading `~` is still in it.
    pub text: String,
    /// Whether any part of the word was quoted.
    pub quoted: bool,
    /// Whether the word has an unquoted `*`, `?`, `[` or `]`: the shell
    /// replaces it with the file names that match, which the analysis
    /// cannot know.
    pub glob: bool,
    /// Whether the word begins with an unquoted `~` for the home directory
    /// (`~` or `~/…`).
    pub home: bool,
}

impl Word {
    /// A word that is exactly this text: nothing in it is expanded.
    #[must_use]
    pub fn literal(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            quoted: false,
            glob: false,
            home: false,
        }
    }

    /// Whether the shell passes the word on exactly as written, apart from
    /// a leading `~`.
    #[must_use]
    pub fn is_fixed(&self) -> bool {
        !self.glob
    }
}

/// A redirection of one simple command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Redirect {
    /// `< file`: the file is read.
    Read(Word),
    /// `> file`, `>> file`: the file is written.
    Write {
        /// The file.
        target: Word,
        /// Whether it is added to (`>>`) or replaced (`>`).
        append: bool,
    },
    /// `2>&1`, `>&2`: one of the command's own streams, again. Nothing
    /// outside the command is touched.
    Duplicate,
}

/// How a simple command is joined to the one before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Joiner {
    /// It is the first.
    First,
    /// `|`: it reads the output of the one before.
    Pipe,
    /// `&&`: it runs if the one before succeeded.
    And,
    /// `||`: it runs if the one before failed.
    Or,
    /// `;` or a line break: it runs after the one before.
    Then,
}

/// One simple command: a program and its arguments, with its redirections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    /// How it is joined to the part before it.
    pub joiner: Joiner,
    /// The program, then its arguments. Never empty.
    pub words: Vec<Word>,
    /// Its redirections.
    pub redirects: Vec<Redirect>,
}

impl Part {
    /// The program's name, as written.
    #[must_use]
    pub fn program(&self) -> &str {
        self.words.first().map_or("", |word| word.text.as_str())
    }

    /// The words after the program.
    #[must_use]
    pub fn arguments(&self) -> &[Word] {
        self.words.get(1..).unwrap_or(&[])
    }
}

/// What the analysis makes of a command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Analysis {
    /// Every part of the line is a simple command the analysis models.
    Parts(Vec<Part>),
    /// The line has something the analysis does not model. No allow rule
    /// and no safe list applies to it.
    NotModelled {
        /// What it was, in words for the approval prompt: "`$`, an
        /// expansion".
        why: String,
    },
}

impl Analysis {
    /// The parts, when the line is modelled.
    #[must_use]
    pub fn parts(&self) -> Option<&[Part]> {
        match self {
            Self::Parts(parts) => Some(parts),
            Self::NotModelled { .. } => None,
        }
    }
}

/// Why a line is not modelled. The tokenizers return it; the public form is
/// [`Analysis::NotModelled`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NotModelled(pub(crate) String);

impl NotModelled {
    pub(crate) fn because(why: impl Into<String>) -> Self {
        Self(why.into())
    }
}

/// Analyses a command line in the language of its shell.
#[must_use]
pub fn analyse(command: &str, syntax: ShellSyntax) -> Analysis {
    let parts = match syntax {
        ShellSyntax::Posix => posix::parts(command),
        ShellSyntax::PowerShell => powershell::parts(command),
        ShellSyntax::Cmd => Err(NotModelled::because(
            "`cmd.exe` commands are not analysed, so each one is asked about",
        )),
        ShellSyntax::Unknown => Err(NotModelled::because(
            "the commands of this shell are not analysed, so each one is asked about",
        )),
    };
    match parts {
        Ok(parts) => Analysis::Parts(parts),
        Err(NotModelled(why)) => Analysis::NotModelled { why },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shell_is_known_by_its_programs_name() {
        for (program, syntax) in [
            ("bash", ShellSyntax::Posix),
            ("/usr/bin/bash", ShellSyntax::Posix),
            ("sh", ShellSyntax::Posix),
            ("dash", ShellSyntax::Posix),
            ("/bin/zsh", ShellSyntax::Posix),
            ("ksh", ShellSyntax::Posix),
            (r"C:\Program Files\Git\bin\bash.exe", ShellSyntax::Posix),
            ("pwsh", ShellSyntax::PowerShell),
            (
                r"C:\Program Files\PowerShell\7\pwsh.exe",
                ShellSyntax::PowerShell,
            ),
            ("PowerShell.EXE", ShellSyntax::PowerShell),
            ("cmd", ShellSyntax::Cmd),
            (r"C:\Windows\System32\cmd.exe", ShellSyntax::Cmd),
            ("CMD.EXE", ShellSyntax::Cmd),
            // Shells whose quoting is their own are not read as POSIX.
            ("fish", ShellSyntax::Unknown),
            ("nu", ShellSyntax::Unknown),
            ("xonsh", ShellSyntax::Unknown),
            ("elvish", ShellSyntax::Unknown),
            ("tcsh", ShellSyntax::Unknown),
            ("bash-wrapper", ShellSyntax::Unknown),
            ("", ShellSyntax::Unknown),
        ] {
            assert_eq!(ShellSyntax::of_program(program), syntax, "{program:?}");
        }
    }

    #[test]
    fn cmd_and_unknown_shells_are_never_modelled() {
        for syntax in [ShellSyntax::Cmd, ShellSyntax::Unknown] {
            for command in ["dir", "ls", "echo hi", ""] {
                let analysis = analyse(command, syntax);
                assert!(analysis.parts().is_none(), "{command:?} in {syntax:?}");
                let Analysis::NotModelled { why } = analysis else {
                    panic!("modelled");
                };
                assert!(why.contains("not analysed"), "{why}");
            }
        }
    }
}
