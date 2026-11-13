//! Which shell runs a command, and how each one is spoken to
//! (`docs/architecture/05-tools.md`, "`shell`", "Which shell").
//!
//! Five kinds are known, and each is handled by its own rules: how the
//! command reaches it, what is put around the command, and how one
//! argument is quoted for it.
//!
//! | Kind | Started as | Around the command |
//! |---|---|---|
//! | `bash` | `bash --noprofile --norc -c <command>` | nothing |
//! | A POSIX shell (`sh`, and any shell not known by name) | `sh -c <command>` | nothing |
//! | PowerShell 7 | `pwsh -NoLogo -NoProfile -NonInteractive -OutputFormat Text -Command <script>` | a first line that sets the output to UTF-8 and turns progress bars off, and after an empty line a last line that makes the exit code the command's |
//! | Windows PowerShell 5.1 | `powershell` with the same arguments | the same |
//! | `cmd.exe` | `cmd /d /e:on /v:off /s /c "<command>"` | the two quotes, which `/s` takes off again |
//!
//! # The command arrives as it was written
//!
//! A permission rule is matched against the command the model wrote, and a
//! person approves that text. So the text the shell reads must be that
//! text, and nothing in it may become part of how the shell was started.
//!
//! - On Linux a process is given its arguments one by one, and the command
//!   is one of them.
//! - `pwsh` and `powershell` read their command line by the C runtime's
//!   rules. The script is one argument, quoted by the standard library by
//!   those same rules, so it arrives whole whatever quotes and backslashes
//!   it holds.
//! - `cmd.exe` reads its command line by rules of its own. The command is
//!   put on the command line as it is, between two quotes that `/s` makes
//!   `cmd.exe` take off. No character of it is escaped, because `cmd.exe`
//!   would read the escapes as part of the command.
//!
//! A command that cannot arrive as written is refused, with the reason:
//! one with a NUL character anywhere, one with a line break for `cmd.exe`
//! (which reads one line), and one too long for the system to pass.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::process::CommandLine;

/// A kind of shell: a set of rules for speaking to one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ShellKind {
    /// GNU bash, on any system: Git Bash is this kind.
    Bash,
    /// A POSIX shell: `sh`, `dash`, `zsh`, `ksh`, and any program not
    /// known by name, which is taken to read `-c <command>` like one.
    Posix,
    /// PowerShell 7, `pwsh`.
    Pwsh,
    /// Windows PowerShell 5.1, `powershell.exe`.
    WindowsPowerShell,
    /// `cmd.exe`.
    Cmd,
}

impl ShellKind {
    /// The kind of the program at `program`, by its file name without the
    /// extension, in any case.
    #[must_use]
    pub fn of(program: &Path) -> Self {
        let stem = program
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        match stem.as_str() {
            "bash" => Self::Bash,
            "pwsh" | "pwsh-preview" => Self::Pwsh,
            "powershell" => Self::WindowsPowerShell,
            "cmd" => Self::Cmd,
            _ => Self::Posix,
        }
    }

    /// Whether this is one of the two PowerShells.
    #[must_use]
    pub fn is_powershell(self) -> bool {
        matches!(self, Self::Pwsh | Self::WindowsPowerShell)
    }

    /// The arguments before the command, unless configuration gives
    /// others.
    fn default_arguments(self) -> &'static [&'static str] {
        match self {
            Self::Bash => &["--noprofile", "--norc", "-c"],
            Self::Posix => &["-c"],
            Self::Pwsh | Self::WindowsPowerShell => &[
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-OutputFormat",
                "Text",
                "-Command",
            ],
            Self::Cmd => &["/d", "/e:on", "/v:off", "/s", "/c"],
        }
    }
}

/// The first line of a PowerShell script.
///
/// - PowerShell writes to a pipe in the console's code page unless told
///   otherwise, which turns every character outside it into `?`. The
///   assignment makes it UTF-8, and is allowed to fail where there is no
///   console to set.
/// - Progress bars are drawn for a terminal, and come through a pipe as
///   noise (as XML, from Windows PowerShell).
const POWERSHELL_FIRST: &str = "$ProgressPreference='SilentlyContinue';\
     try{[Console]::OutputEncoding=[System.Text.UTF8Encoding]::new($false)}catch{}";

/// The last line of a PowerShell script.
///
/// `-Command` exits with 0 or 1 whatever a program the script ran exited
/// with. This makes the exit code what a POSIX shell's would be: 0 when
/// the last statement succeeded, else the exit code of the last program
/// that was run, else 1.
///
/// A command cannot make this line part of itself. A back-tick at its end
/// joins the empty line that is put between them. Anything else that runs
/// on into the next line (an open quote, an open bracket, a `|` at the
/// end) is a script that does not parse, and PowerShell then runs none of
/// it. One thing the first line costs: a command that begins with a
/// `using` statement fails, because PowerShell wants those before
/// everything else. Such a command belongs in a script file.
const POWERSHELL_LAST: &str = "if($?){exit 0};if($LASTEXITCODE){exit $LASTEXITCODE};exit 1";

/// The most characters of a command line Windows passes to a process, less
/// room for the program and its arguments.
const WINDOWS_COMMAND_CHARS: usize = 32_000;
/// The most characters `cmd.exe` reads of its own command line, less the
/// same.
const CMD_COMMAND_CHARS: usize = 8_000;
/// The most bytes Linux passes as one argument (128 KiB), less a margin.
const UNIX_COMMAND_BYTES: usize = 120_000;

/// Why a command cannot be given to the shell as it was written.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Unrunnable {
    /// The command is empty or all white space.
    #[error("The command is empty.")]
    Empty,
    /// A NUL character ends a command line wherever it is.
    #[error(
        "The command has a NUL character in it, which no command line can carry. Remove it, \
         or write the bytes to a file and use that."
    )]
    Nul,
    /// `cmd.exe` reads the first line of what it is given and no more.
    #[error(
        "The command has a line break in it, and cmd.exe reads one line: everything after \
         the first line would be dropped. Write it as one line, with `&` or `&&` between \
         commands."
    )]
    LineBreak,
    /// The system would refuse to start a command line this long.
    #[error(
        "The command is too long to pass to {shell}: it has {length} {unit} and the limit is \
         about {limit}. Put it in a script file and run that."
    )]
    TooLong {
        /// The shell it was for.
        shell: String,
        /// How long it is.
        length: usize,
        /// What `length` and `limit` count.
        unit: &'static str,
        /// The most that can be passed.
        limit: usize,
    },
}

/// Why a piece of text cannot be made one argument for a shell.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Unquotable {
    /// A NUL character cannot be in an argument.
    #[error("the text has a NUL character in it")]
    Nul,
    /// `cmd.exe` has no way to write this character so that every program
    /// reads it back: a quote, a percent sign, or a line break.
    #[error("cmd.exe cannot carry `{0}` in an argument unchanged")]
    NotForCmd(String),
}

/// A shell: its kind, its program, and what is known of its version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shell {
    kind: ShellKind,
    program: PathBuf,
    arguments: Option<Vec<String>>,
    version: Option<String>,
}

impl Shell {
    /// The shell at `program`, of the kind its name says
    /// ([`ShellKind::of`]).
    #[must_use]
    pub fn new(program: impl Into<PathBuf>) -> Self {
        let program = program.into();
        Self::of_kind(ShellKind::of(&program), program)
    }

    /// The shell at `program`, spoken to as `kind` whatever its name.
    #[must_use]
    pub fn of_kind(kind: ShellKind, program: impl Into<PathBuf>) -> Self {
        let version = match kind {
            // The one version of it there is on a supported Windows.
            ShellKind::WindowsPowerShell => Some("5.1".to_owned()),
            _ => None,
        };
        Self {
            kind,
            program: program.into(),
            arguments: None,
            version,
        }
    }

    /// Sets the arguments that come before the command, in place of the
    /// kind's own. The command is always passed after them, last.
    #[must_use]
    pub fn with_arguments(mut self, arguments: Vec<String>) -> Self {
        self.arguments = Some(arguments);
        self
    }

    /// Sets the version, as [`Shell::probe_version`] found it.
    #[must_use]
    pub fn with_version(mut self, version: impl Into<String>) -> Self {
        self.version = Some(version.into());
        self
    }

    /// The kind.
    #[must_use]
    pub fn kind(&self) -> ShellKind {
        self.kind
    }

    /// The program.
    #[must_use]
    pub fn program(&self) -> &Path {
        &self.program
    }

    /// The version, when it is known.
    #[must_use]
    pub fn version(&self) -> Option<&str> {
        self.version.as_deref()
    }

    /// What the model is told it is writing for: `bash 5.2.21`,
    /// `PowerShell 7.4.6 (pwsh)`, `Windows PowerShell 5.1`, `cmd.exe`.
    #[must_use]
    pub fn name(&self) -> String {
        let version = |fallback: &str| self.version.as_deref().unwrap_or(fallback).to_owned();
        match self.kind {
            ShellKind::Bash => match &self.version {
                Some(version) => format!("bash {version}"),
                None => "bash".to_owned(),
            },
            ShellKind::Posix => {
                let name = self
                    .program
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .unwrap_or("sh");
                match &self.version {
                    Some(version) => format!("{name} {version} (a POSIX shell)"),
                    None => format!("{name} (a POSIX shell)"),
                }
            }
            ShellKind::Pwsh => format!("PowerShell {} (pwsh)", version("7")),
            ShellKind::WindowsPowerShell => format!("Windows PowerShell {}", version("5.1")),
            ShellKind::Cmd => "cmd.exe".to_owned(),
        }
    }

    /// The command line that runs `command` in this shell, or why the
    /// command cannot be run as written.
    pub fn command_line(&self, command: &str) -> Result<CommandLine, Unrunnable> {
        self.command_line_for(command, cfg!(windows))
    }

    /// [`command_line`](Self::command_line) for a named system, so that
    /// the rules of both can be tested on either.
    pub(crate) fn command_line_for(
        &self,
        command: &str,
        windows: bool,
    ) -> Result<CommandLine, Unrunnable> {
        if command.trim().is_empty() {
            return Err(Unrunnable::Empty);
        }
        if command.contains('\0') {
            return Err(Unrunnable::Nul);
        }
        if self.kind == ShellKind::Cmd && command.contains(['\n', '\r']) {
            return Err(Unrunnable::LineBreak);
        }
        let text = if self.kind.is_powershell() {
            // The empty line is for a command that ends with a back-tick,
            // which joins the next line to its own: the next line is then
            // nothing, and the harness's last line stays a line of its own.
            format!("{POWERSHELL_FIRST}\n{command}\n\n{POWERSHELL_LAST}")
        } else {
            command.to_owned()
        };
        let (length, unit, limit) = if windows {
            // Windows counts a command line in UTF-16 units.
            let units = text.encode_utf16().count();
            if self.kind == ShellKind::Cmd {
                (units, "characters", CMD_COMMAND_CHARS)
            } else {
                // Quoted by the C runtime's rules, a quote takes two
                // characters, and so may a backslash.
                let escapes = text.chars().filter(|c| matches!(c, '"' | '\\')).count();
                (units + escapes, "characters", WINDOWS_COMMAND_CHARS)
            }
        } else {
            (text.len(), "bytes", UNIX_COMMAND_BYTES)
        };
        if length > limit {
            return Err(Unrunnable::TooLong {
                shell: self.name(),
                length,
                unit,
                limit,
            });
        }

        let mut line = CommandLine::new(&self.program);
        line = match &self.arguments {
            Some(arguments) => line.args(arguments.iter().cloned()),
            None => line.args(self.kind.default_arguments().iter().copied()),
        };
        Ok(if self.kind == ShellKind::Cmd {
            // As it is, between the two quotes that `/s` takes off.
            line.verbatim(format!("\"{text}\""))
        } else {
            line.arg(text)
        })
    }

    /// Writes `text` as one word of this shell's language: what to put in
    /// a command so that the shell reads exactly `text`.
    ///
    /// What a program started by the shell then receives is the shell's
    /// doing. bash, a POSIX shell and PowerShell 7 hand a program the text.
    /// Windows PowerShell 5.1 is known to drop a double quote and an empty
    /// text on the way to a program, whatever the quoting, so there a text
    /// with either is for PowerShell's own commands only.
    ///
    /// - A POSIX shell and bash: single quotes, in which nothing is
    ///   special; a single quote inside is written `'\''`.
    /// - Both PowerShells: single quotes, with a single quote inside
    ///   doubled. PowerShell also reads four typographic quote characters
    ///   as single quotes, and those are doubled too.
    /// - `cmd.exe`: double quotes, in which `& | < > ^ ( )` are not
    ///   special. Backslashes at the end are doubled, for the program's
    ///   own reading of the quote after them. A text with a `"`, a `%` or
    ///   a line break is refused: `cmd.exe` expands `%NAME%` inside quotes
    ///   too, and has no way to write the other two that every program
    ///   reads back.
    pub fn quote(&self, text: &str) -> Result<String, Unquotable> {
        if text.contains('\0') {
            return Err(Unquotable::Nul);
        }
        match self.kind {
            ShellKind::Bash | ShellKind::Posix => Ok(format!("'{}'", text.replace('\'', "'\\''"))),
            ShellKind::Pwsh | ShellKind::WindowsPowerShell => {
                let mut quoted = String::with_capacity(text.len() + 2);
                quoted.push('\'');
                for character in text.chars() {
                    quoted.push(character);
                    if POWERSHELL_SINGLE_QUOTES.contains(&character) {
                        quoted.push(character);
                    }
                }
                quoted.push('\'');
                Ok(quoted)
            }
            ShellKind::Cmd => {
                for (refused, shown) in [
                    ('"', "\""),
                    ('%', "%"),
                    ('\n', "a line break"),
                    ('\r', "a line break"),
                ] {
                    if text.contains(refused) {
                        return Err(Unquotable::NotForCmd(shown.to_owned()));
                    }
                }
                let trailing = text.chars().rev().take_while(|c| *c == '\\').count();
                Ok(format!("\"{text}{}\"", "\\".repeat(trailing)))
            }
        }
    }

    /// Runs the shell once to ask its version, and remembers the answer.
    ///
    /// It starts a process and blocks until that ends, or for `limit` at
    /// the most: about a millisecond for bash, and a few hundred for
    /// PowerShell 7, so a harness does it off its start-up path. A shell
    /// that does not say, or says nothing this can read, stays without a
    /// version. Windows PowerShell is 5.1 and `cmd.exe` has none worth
    /// telling, so neither is started.
    pub fn probe_version(&mut self, limit: std::time::Duration) {
        let arguments: &[&str] = match self.kind {
            ShellKind::Bash | ShellKind::Pwsh => &["--version"],
            ShellKind::Posix | ShellKind::WindowsPowerShell | ShellKind::Cmd => return,
        };
        let Ok(said) = crate::process::brief::run(&self.program, arguments, None, &[], limit)
        else {
            return;
        };
        let text = String::from_utf8_lossy(&said);
        if let Some(version) = parse_version(self.kind, &text) {
            self.version = Some(version);
        }
    }
}

/// The characters PowerShell reads as a single quote.
const POWERSHELL_SINGLE_QUOTES: [char; 5] = ['\'', '\u{2018}', '\u{2019}', '\u{201a}', '\u{201b}'];

/// The version in what a shell printed for `--version`.
fn parse_version(kind: ShellKind, text: &str) -> Option<String> {
    let line = text.lines().find(|line| !line.trim().is_empty())?;
    let word = match kind {
        // `GNU bash, version 5.2.21(1)-release (x86_64-pc-linux-gnu)`
        ShellKind::Bash => line.split_once("version ")?.1.split(['(', ' ']).next()?,
        // `PowerShell 7.4.6`
        ShellKind::Pwsh => line.trim().rsplit(' ').next()?,
        _ => return None,
    };
    let plausible = word.starts_with(|c: char| c.is_ascii_digit())
        && word.len() <= 32
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'));
    plausible.then(|| word.to_owned())
}

/// Why no shell was found.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ShellError {
    /// The program configuration names is not there.
    #[error("the configured shell `{0}` was not found, as a path or on PATH")]
    Configured(String),
    /// The program configuration names is a relative path (`bin/bash`).
    /// It would be found from wherever the harness was started, which is
    /// usually the workspace, whose files are not the user's to vouch for.
    #[error(
        "the configured shell `{0}` is a relative path: give the name of a program on PATH, or \
         an absolute path"
    )]
    NotAbsolute(String),
    /// None of the shells this system should have was found.
    #[error("no shell was found: looked for {0}")]
    NoneFound(String),
}

/// Where shells are looked for: the parts of the environment that say.
///
/// It is a value, so that the search for each system's shells can be
/// tested on the other: [`ShellSearch::from_env`] is the real one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellSearch {
    /// Whether to look for the shells of Windows, and for programs by
    /// Windows's rules (`.exe`, any case).
    pub windows: bool,
    /// The directories of `PATH`, in order. Only the absolute ones are
    /// looked in: an empty or relative entry (`.`, `bin`,
    /// `./node_modules/.bin`) names a directory from wherever the harness
    /// was started, which is usually the workspace, and a file there named
    /// `bash` must not become the shell every command runs in.
    pub path: Vec<PathBuf>,
    /// `%SystemRoot%`, where Windows keeps `cmd.exe` and Windows
    /// PowerShell. Used only when it is absolute.
    pub system_root: Option<PathBuf>,
    /// `%ComSpec%`, the system's own word for where `cmd.exe` is. Used only
    /// when it is absolute.
    pub comspec: Option<PathBuf>,
}

impl ShellSearch {
    /// The search this process's environment describes.
    #[must_use]
    pub fn from_env() -> Self {
        let variable = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
        Self {
            windows: cfg!(windows),
            path: variable("PATH")
                .map(|path| std::env::split_paths(&path).collect())
                .unwrap_or_default(),
            system_root: variable("SystemRoot").map(PathBuf::from),
            comspec: variable("ComSpec").map(PathBuf::from),
        }
    }

    /// Finds the shell to use.
    ///
    /// - With `program` set (configuration's `shell.program`): that
    ///   program, as an absolute path or a name on `PATH`, and nothing
    ///   else. Its kind is what its name says. A relative path is refused
    ///   ([`ShellError::NotAbsolute`]).
    /// - Otherwise, on Linux: `bash`, then `sh`.
    /// - Otherwise, on Windows: PowerShell 7 (`pwsh`), then Windows
    ///   PowerShell, then `cmd.exe`.
    ///
    /// `arguments` (configuration's `shell.args`) replace the kind's own
    /// arguments before the command.
    ///
    /// **What is found is an absolute path, as found.** Only absolute
    /// directories are looked in, so nothing of the workspace is found. A
    /// link is not resolved here: a program that is several programs
    /// (`busybox`) knows which one to be from the name it is started by. The
    /// harness keys what it keeps about the shell by the resolved path.
    ///
    /// **On Windows this search is the harness's own, not the system's.**
    /// It tries a name with `.exe` and as given, and never the other
    /// extensions of `PATHEXT` (a `pwsh.bat` is not PowerShell). It never
    /// looks in the current directory, which the system's search for a
    /// program, and `cmd.exe`'s for a command, look in first.
    ///
    /// It looks at the file system and blocks for as long as that takes.
    /// It starts no process: [`Shell::probe_version`] does.
    pub fn find(
        &self,
        program: Option<&str>,
        arguments: Option<&[String]>,
    ) -> Result<Shell, ShellError> {
        let shell = match program {
            Some(program) => {
                let found = if program.contains(['/', '\\']) {
                    if !Path::new(program).is_absolute() {
                        return Err(ShellError::NotAbsolute(program.to_owned()));
                    }
                    is_program(Path::new(program), self.windows).then(|| PathBuf::from(program))
                } else {
                    self.on_path(program)
                };
                Shell::new(found.ok_or_else(|| ShellError::Configured(program.to_owned()))?)
            }
            None if self.windows => self.find_windows()?,
            None => self
                .on_path("bash")
                .or_else(|| self.on_path("sh"))
                .map(Shell::new)
                .ok_or_else(|| ShellError::NoneFound("bash and sh on PATH".to_owned()))?,
        };
        Ok(match arguments {
            Some(arguments) => shell.with_arguments(arguments.to_vec()),
            None => shell,
        })
    }

    fn find_windows(&self) -> Result<Shell, ShellError> {
        if let Some(pwsh) = self.on_path("pwsh") {
            return Ok(Shell::of_kind(ShellKind::Pwsh, pwsh));
        }
        let system = |tail: &[&str]| {
            let mut path = self.system_root.clone().filter(|root| root.is_absolute())?;
            path.extend(tail);
            is_program(&path, true).then_some(path)
        };
        if let Some(powershell) = self
            .on_path("powershell")
            .or_else(|| system(&["System32", "WindowsPowerShell", "v1.0", "powershell.exe"]))
        {
            return Ok(Shell::of_kind(ShellKind::WindowsPowerShell, powershell));
        }
        let cmd = self
            .comspec
            .clone()
            .filter(|path| {
                path.is_absolute()
                    && is_program(path, true)
                    && ShellKind::of(path) == ShellKind::Cmd
            })
            .or_else(|| system(&["System32", "cmd.exe"]))
            .or_else(|| self.on_path("cmd"));
        cmd.map(|cmd| Shell::of_kind(ShellKind::Cmd, cmd))
            .ok_or_else(|| {
                ShellError::NoneFound(
                    "pwsh and powershell on PATH, and powershell.exe and cmd.exe under \
                     %SystemRoot%"
                        .to_owned(),
                )
            })
    }

    /// The first program called `name` in the absolute directories of
    /// `PATH`. Whether a directory is absolute is this system's question,
    /// as the directories are this system's.
    fn on_path(&self, name: &str) -> Option<PathBuf> {
        let mut names = vec![OsString::from(name)];
        if self.windows && Path::new(name).extension().is_none() {
            names.insert(0, OsString::from(format!("{name}.exe")));
        }
        self.path
            .iter()
            .filter(|directory| directory.is_absolute())
            .flat_map(|directory| names.iter().map(move |name| directory.join(name)))
            .find(|candidate| is_program(candidate, self.windows))
    }
}

/// Whether `path` is a file that can be run.
#[allow(
    clippy::disallowed_methods,
    reason = "the search for a shell runs once, off the core thread, and blocks by design"
)]
fn is_program(path: &Path, windows: bool) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    if !windows {
        use std::os::unix::fs::PermissionsExt;
        return metadata.permissions().mode() & 0o111 != 0;
    }
    let _ = windows;
    true
}

#[cfg(test)]
mod tests {
    use cortex_base::Rng;
    use cortex_testkit::cases::{self, ensure};

    use super::*;
    use crate::process::Argument;

    fn exact(arguments: &[&str]) -> Vec<Argument> {
        arguments
            .iter()
            .map(|one| Argument::Exact((*one).to_owned()))
            .collect()
    }

    #[test]
    fn a_shell_is_known_by_its_programs_name() {
        for (program, kind) in [
            ("/usr/bin/bash", ShellKind::Bash),
            ("C:\\Program Files\\Git\\bin\\BASH.EXE", ShellKind::Bash),
            ("sh", ShellKind::Posix),
            ("/bin/dash", ShellKind::Posix),
            ("/usr/bin/zsh", ShellKind::Posix),
            ("something-else", ShellKind::Posix),
            ("pwsh", ShellKind::Pwsh),
            (
                "C:\\Program Files\\PowerShell\\7\\pwsh.exe",
                ShellKind::Pwsh,
            ),
            ("PowerShell.exe", ShellKind::WindowsPowerShell),
            ("C:\\Windows\\System32\\cmd.exe", ShellKind::Cmd),
            ("CMD", ShellKind::Cmd),
        ] {
            // A Windows path is one file name to Linux, so only the name
            // is given to the test where the separators differ.
            let name = program.rsplit(['/', '\\']).next().expect("a name");
            assert_eq!(ShellKind::of(Path::new(name)), kind, "{program}");
        }
        assert!(ShellKind::Pwsh.is_powershell() && ShellKind::WindowsPowerShell.is_powershell());
        assert!(!ShellKind::Cmd.is_powershell() && !ShellKind::Bash.is_powershell());
    }

    #[test]
    fn the_model_is_told_the_shell_and_its_version() {
        assert_eq!(Shell::new("bash").name(), "bash");
        assert_eq!(
            Shell::new("bash").with_version("5.2.21").name(),
            "bash 5.2.21"
        );
        assert_eq!(Shell::new("/bin/sh").name(), "sh (a POSIX shell)");
        assert_eq!(Shell::new("zsh").name(), "zsh (a POSIX shell)");
        assert_eq!(Shell::new("pwsh").name(), "PowerShell 7 (pwsh)");
        assert_eq!(
            Shell::new("pwsh").with_version("7.4.6").name(),
            "PowerShell 7.4.6 (pwsh)"
        );
        assert_eq!(Shell::new("powershell").name(), "Windows PowerShell 5.1");
        assert_eq!(Shell::new("powershell").version(), Some("5.1"));
        assert_eq!(Shell::new("cmd").name(), "cmd.exe");
    }

    #[test]
    fn a_version_is_read_from_what_the_shell_prints() {
        assert_eq!(
            parse_version(
                ShellKind::Bash,
                "GNU bash, version 5.2.21(1)-release (x86_64-pc-linux-gnu)\nCopyright\n"
            ),
            Some("5.2.21".to_owned())
        );
        assert_eq!(
            parse_version(
                ShellKind::Bash,
                "GNU bash, version 3.2.57(1)-release (arm64)"
            ),
            Some("3.2.57".to_owned())
        );
        assert_eq!(
            parse_version(ShellKind::Pwsh, "PowerShell 7.4.6\r\n"),
            Some("7.4.6".to_owned())
        );
        assert_eq!(
            parse_version(ShellKind::Pwsh, "\nPowerShell 7.5.0-preview.3\n"),
            Some("7.5.0-preview.3".to_owned())
        );
        // Not a version: nothing is claimed.
        assert_eq!(
            parse_version(ShellKind::Bash, "bash: --version: invalid option"),
            None
        );
        assert_eq!(parse_version(ShellKind::Pwsh, "not found; rm -rf"), None);
        assert_eq!(parse_version(ShellKind::Pwsh, ""), None);
        assert_eq!(
            parse_version(ShellKind::Cmd, "Microsoft Windows 10.0"),
            None
        );
    }

    #[test]
    fn each_kind_is_started_by_its_own_rules() {
        let line = Shell::new("bash")
            .command_line("echo hi")
            .expect("runnable");
        assert_eq!(line.program, Path::new("bash"));
        assert_eq!(
            line.arguments,
            exact(&["--noprofile", "--norc", "-c", "echo hi"])
        );

        let line = Shell::new("sh").command_line("echo hi").expect("runnable");
        assert_eq!(line.arguments, exact(&["-c", "echo hi"]));

        for program in ["pwsh", "powershell"] {
            let line = Shell::new(program)
                .command_line("Write-Output \"a b\"")
                .expect("runnable");
            let (script, flags) = line.arguments.split_last().expect("arguments");
            assert_eq!(
                flags,
                exact(&[
                    "-NoLogo",
                    "-NoProfile",
                    "-NonInteractive",
                    "-OutputFormat",
                    "Text",
                    "-Command"
                ])
            );
            // One argument: the command on a line of its own, untouched,
            // between the two lines of the harness.
            let Argument::Exact(script) = script else {
                panic!("the script is quoted by the standard library");
            };
            let lines: Vec<&str> = script.split('\n').collect();
            assert_eq!(lines.len(), 4);
            assert_eq!(lines[0], POWERSHELL_FIRST);
            assert_eq!(lines[1], "Write-Output \"a b\"");
            // An empty line, so that a command ending in a back-tick
            // cannot join the last line to itself.
            assert_eq!(lines[2], "");
            assert_eq!(lines[3], POWERSHELL_LAST);
        }

        let line = Shell::new("cmd")
            .command_line("echo a\"b & dir")
            .expect("runnable");
        let (command, flags) = line.arguments.split_last().expect("arguments");
        assert_eq!(flags, exact(&["/d", "/e:on", "/v:off", "/s", "/c"]));
        // Verbatim, with nothing escaped: `cmd.exe` would read an escape
        // as part of the command.
        assert_eq!(
            command,
            &Argument::Verbatim("\"echo a\"b & dir\"".to_owned())
        );
    }

    #[test]
    fn configured_arguments_replace_the_kinds_own() {
        let line = Shell::new("bash")
            .with_arguments(vec!["-l".to_owned(), "-c".to_owned()])
            .command_line("true")
            .expect("runnable");
        assert_eq!(line.arguments, exact(&["-l", "-c", "true"]));
        let line = Shell::new("cmd")
            .with_arguments(vec!["/c".to_owned()])
            .command_line("ver")
            .expect("runnable");
        assert_eq!(
            line.arguments,
            vec![
                Argument::Exact("/c".to_owned()),
                Argument::Verbatim("\"ver\"".to_owned())
            ]
        );
    }

    #[test]
    fn a_command_that_cannot_arrive_as_written_is_refused() {
        for shell in ["bash", "sh", "pwsh", "powershell", "cmd"] {
            let shell = Shell::new(shell);
            assert_eq!(shell.command_line(""), Err(Unrunnable::Empty));
            assert_eq!(shell.command_line(" \n\t"), Err(Unrunnable::Empty));
            assert_eq!(shell.command_line("echo a\0b"), Err(Unrunnable::Nul));
        }
        // One line for cmd.exe, which would drop the rest; any number for
        // the others.
        assert_eq!(
            Shell::new("cmd").command_line("echo a\necho b"),
            Err(Unrunnable::LineBreak)
        );
        assert_eq!(
            Shell::new("cmd").command_line("echo a\r"),
            Err(Unrunnable::LineBreak)
        );
        for shell in ["bash", "sh", "pwsh", "powershell"] {
            assert!(Shell::new(shell).command_line("echo a\necho b").is_ok());
        }
        let error = Shell::new("cmd")
            .command_line("echo a\necho b")
            .expect_err("refused");
        assert!(error.to_string().contains("one line"), "{error}");
    }

    #[test]
    fn a_command_too_long_to_pass_is_refused_with_the_numbers() {
        let long = "x".repeat(9_000);
        // Fine for everything but cmd.exe.
        assert!(Shell::new("bash").command_line_for(&long, false).is_ok());
        assert!(Shell::new("pwsh").command_line_for(&long, true).is_ok());
        let error = Shell::new("cmd")
            .command_line_for(&long, true)
            .expect_err("too long for cmd.exe");
        assert_eq!(
            error,
            Unrunnable::TooLong {
                shell: "cmd.exe".to_owned(),
                length: 9_000,
                unit: "characters",
                limit: 8_000
            }
        );
        assert!(error.to_string().contains("9000 characters"), "{error}");

        let longer = "x".repeat(40_000);
        assert!(Shell::new("bash").command_line_for(&longer, false).is_ok());
        assert!(matches!(
            Shell::new("pwsh").command_line_for(&longer, true),
            Err(Unrunnable::TooLong { limit: 32_000, .. })
        ));
        let longest = "x".repeat(130_000);
        assert!(matches!(
            Shell::new("bash").command_line_for(&longest, false),
            Err(Unrunnable::TooLong {
                unit: "bytes",
                limit: 120_000,
                ..
            })
        ));
    }

    #[test]
    fn quoting_follows_each_shells_rules() {
        let bash = Shell::new("bash");
        assert_eq!(bash.quote("plain").as_deref(), Ok("'plain'"));
        assert_eq!(bash.quote("").as_deref(), Ok("''"));
        assert_eq!(bash.quote("it's").as_deref(), Ok("'it'\\''s'"));
        assert_eq!(
            bash.quote("a b; rm -rf / & $(x) `y` \"z\" \\ \n")
                .as_deref(),
            Ok("'a b; rm -rf / & $(x) `y` \"z\" \\ \n'")
        );
        assert_eq!(Shell::new("sh").quote("it's"), bash.quote("it's"));

        for program in ["pwsh", "powershell"] {
            let shell = Shell::new(program);
            assert_eq!(shell.quote("plain").as_deref(), Ok("'plain'"));
            assert_eq!(shell.quote("it's").as_deref(), Ok("'it''s'"));
            assert_eq!(
                shell.quote("$x; `n \"q\" & | %PATH% ^").as_deref(),
                Ok("'$x; `n \"q\" & | %PATH% ^'")
            );
            // PowerShell ends a single-quoted string at a typographic
            // quote too, so each of those is doubled like the plain one.
            assert_eq!(
                shell
                    .quote("a\u{2018}b\u{2019}c\u{201a}d\u{201b}e")
                    .as_deref(),
                Ok("'a\u{2018}\u{2018}b\u{2019}\u{2019}c\u{201a}\u{201a}d\u{201b}\u{201b}e'")
            );
        }

        let cmd = Shell::new("cmd");
        assert_eq!(cmd.quote("plain").as_deref(), Ok("\"plain\""));
        assert_eq!(
            cmd.quote("a & b | c ^ d < e > f (g)").as_deref(),
            Ok("\"a & b | c ^ d < e > f (g)\"")
        );
        // Backslashes before the closing quote are doubled, so that the
        // program does not read the quote as escaped.
        assert_eq!(cmd.quote("C:\\dir\\").as_deref(), Ok("\"C:\\dir\\\\\""));
        assert_eq!(cmd.quote("a\\b").as_deref(), Ok("\"a\\b\""));
        for refused in ["say \"hi\"", "%PATH%", "100%", "two\nlines", "cr\r"] {
            assert!(
                matches!(cmd.quote(refused), Err(Unquotable::NotForCmd(_))),
                "{refused:?}"
            );
        }
        for shell in ["bash", "pwsh", "cmd"] {
            assert_eq!(Shell::new(shell).quote("a\0b"), Err(Unquotable::Nul));
        }
    }

    /// A quoted text is one token of the shell's language: read back by
    /// the shell's own rules for a quoted string, it is the text.
    #[test]
    fn a_quoted_text_reads_back_as_itself() {
        // The readers below are the two languages' rules for a quoted
        // string, written from their documentation, not from `quote`.
        fn read_posix(quoted: &str) -> Option<String> {
            // A word: single-quoted pieces, and `\'` between them.
            let mut out = String::new();
            let mut rest = quoted;
            while !rest.is_empty() {
                if let Some(after) = rest.strip_prefix("\\'") {
                    out.push('\'');
                    rest = after;
                } else {
                    let after = rest.strip_prefix('\'')?;
                    let end = after.find('\'')?;
                    out.push_str(&after[..end]);
                    rest = &after[end + 1..];
                }
            }
            Some(out)
        }
        fn read_powershell(quoted: &str) -> Option<String> {
            let is_quote = |c: char| POWERSHELL_SINGLE_QUOTES.contains(&c);
            let mut characters = quoted.chars().peekable();
            if !is_quote(characters.next()?) {
                return None;
            }
            let mut out = String::new();
            loop {
                let character = characters.next()?;
                if is_quote(character) {
                    match characters.peek() {
                        // A doubled quote is one quote: the first of the two.
                        Some(next) if is_quote(*next) => {
                            out.push(character);
                            characters.next();
                        }
                        // The string ends here, and so must the token.
                        Some(_) => return None,
                        None => return Some(out),
                    }
                } else {
                    out.push(character);
                }
            }
        }

        cases::check(
            "quoted_text_reads_back",
            400,
            |rng| {
                let alphabet = [
                    "a", " ", "'", "\"", "\\", "$", "`", "&", "|", "^", "%", ";", "\n", "\u{2018}",
                    "\u{2019}", "\u{201a}", "\u{201b}", "é", "(", ")", "<", ">", "!", "*", "~",
                ];
                (0..rng.below(14))
                    .map(|_| alphabet[rng.below(alphabet.len() as u64) as usize])
                    .collect::<String>()
            },
            |text| {
                let posix = Shell::new("sh").quote(text)?;
                ensure!(
                    read_posix(&posix).as_deref() == Some(text.as_str()),
                    "sh reads {posix:?} as {:?}",
                    read_posix(&posix)
                );
                let powershell = Shell::new("pwsh").quote(text)?;
                ensure!(
                    read_powershell(&powershell).as_deref() == Some(text.as_str()),
                    "PowerShell reads {powershell:?} as {:?}",
                    read_powershell(&powershell)
                );
                // cmd.exe: quoted or refused, never passed through bare.
                match Shell::new("cmd").quote(text) {
                    Ok(quoted) => {
                        ensure!(
                            quoted.starts_with('"') && quoted.ends_with('"'),
                            "not quoted: {quoted:?}"
                        );
                        let inside = &quoted[1..quoted.len() - 1];
                        ensure!(
                            !inside.contains(['"', '%', '\n', '\r']),
                            "cmd.exe would change {quoted:?}"
                        );
                        ensure!(inside.starts_with(text.as_str()), "the text is not there");
                    }
                    Err(_) => ensure!(
                        text.contains(['"', '%', '\n', '\r']),
                        "{text:?} was refused for no reason"
                    ),
                }
                Ok(())
            },
        );
    }
}
