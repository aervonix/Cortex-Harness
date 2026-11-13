//! The command line, read into what to do (decision 0011: `lexopt`, and the
//! matching, the help and the errors written here).
//!
//! The grammar is small. A first word that names a subcommand
//! (`sessions`, `config`, `permissions`, `auth`) chooses it, and the rest is
//! that subcommand's. Anything else is a run: flags, and words that are the
//! prompt. A run is headless with `-p`, or when a prompt is given and
//! standard output is not a terminal; otherwise it is the interactive
//! application, which is not built yet (`docs/architecture/10-headless.md`,
//! "Invocation").
//!
//! [`FLAGS`] lists every flag of a run once. The help is printed from it,
//! and a test parses every entry, so the two cannot disagree.

use std::ffi::OsString;

use lexopt::{Arg, Parser, ValueExt};

/// The hidden subcommand whose body is the guard of process trees
/// (`cortex_sandbox::tree::run_guard`). The core starts the harness's own
/// executable with it. It is not in the help.
pub(crate) const GUARD: &str = "__tree-guard";

/// A flag of a run, as the help shows it.
pub(crate) struct Flag {
    /// The names: `-p, --print`.
    pub(crate) names: &'static str,
    /// What it takes, if anything: `MODEL`.
    pub(crate) value: Option<&'static str>,
    /// One line of help.
    pub(crate) help: &'static str,
}

const fn flag(names: &'static str, value: Option<&'static str>, help: &'static str) -> Flag {
    Flag { names, value, help }
}

/// Every flag a run takes, in the order the help shows them.
pub(crate) const FLAGS: &[Flag] = &[
    flag(
        "-p, --print",
        None,
        "Run headless: the prompt is the words given, standard input, or both",
    ),
    flag(
        "--output",
        Some("FORMAT"),
        "text (the default), json, or jsonl: a line per event",
    ),
    flag(
        "--include-deltas",
        None,
        "With jsonl: also write streaming deltas",
    ),
    flag(
        "--continue",
        None,
        "Continue the latest session of this workspace",
    ),
    flag("--resume", Some("ID"), "Continue the session ID"),
    flag(
        "--fork",
        Some("ID"),
        "Start a new session from a copy of the session ID",
    ),
    flag("--no-session", None, "Keep the session in memory: no log"),
    flag("--model", Some("MODEL"), "The main model, for this run"),
    flag(
        "--effort",
        Some("LEVEL"),
        "The reasoning level: minimal, low, medium, high, max, or the model's own",
    ),
    flag(
        "--speed",
        Some("TIER"),
        "The speed tier: standard, or fast where the model offers it",
    ),
    flag(
        "--permission-mode",
        Some("MODE"),
        "read-only, supervised, autonomous or unrestricted",
    ),
    flag(
        "--allow",
        Some("RULE"),
        "Allow what a rule matches, for this run: 'exec:cargo test *'",
    ),
    flag(
        "--deny",
        Some("RULE"),
        "Deny what a rule matches, for this run: 'write src/generated/**'",
    ),
    flag(
        "--trust-project",
        None,
        "Treat the project as trusted, for this run",
    ),
    flag(
        "--fail-on-denial",
        None,
        "End the run at the first call nobody can approve (exit code 6)",
    ),
    flag(
        "--max-requests",
        Some("N"),
        "Stop after N requests to a model (exit code 7)",
    ),
    flag(
        "--max-cost",
        Some("USD"),
        "Stop when the run has cost this many US dollars (exit code 7)",
    ),
    flag(
        "--max-time",
        Some("DURATION"),
        "Stop after this long, as in 90s or 10m (exit code 7)",
    ),
    flag(
        "--system-append",
        Some("TEXT"),
        "Add text to the end of the system prompt",
    ),
    flag(
        "--profile",
        Some("NAME"),
        "Load configuration with the profile NAME",
    ),
    flag(
        "-c",
        Some("KEY=VALUE"),
        "Set a configuration key for this run: -c retry.attempts=3",
    ),
    flag(
        "--log-level",
        Some("LEVEL"),
        "What the log file takes: error, warn, info, debug, trace",
    ),
    flag("-q, --quiet", None, "Write nothing to standard error"),
    flag(
        "-v, --verbose",
        None,
        "Write more to standard error: what each step does",
    ),
    flag("-h, --help", None, "Print this help"),
    flag("-V, --version", None, "Print the version"),
];

/// What the command line asks for.
#[derive(Debug, PartialEq)]
pub(crate) enum Command {
    /// The help of the program, or of a subcommand.
    Help(Topic),
    /// The version.
    Version,
    /// A run: headless, or the interactive application.
    Run(Box<Run>),
    /// `cortex sessions …`
    Sessions(SessionsCommand),
    /// `cortex config …`
    Config(ConfigCommand),
    /// `cortex permissions …`
    Permissions(PermissionsCommand),
    /// `cortex auth …`
    Auth(AuthCommand),
    /// `cortex models`
    Models,
}

/// Whose help to print.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Topic {
    Main,
    Sessions,
    Config,
    Permissions,
    Auth,
    Models,
}

/// What a headless run writes on standard output.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum Format {
    /// The assistant's last message, as plain text.
    #[default]
    Text,
    /// One JSON object when the run ends.
    Json,
    /// A JSON object per line, as things happen.
    Jsonl,
}

/// Which session a run is in.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) enum Choice {
    /// A new one.
    #[default]
    New,
    /// The latest of this workspace.
    Continue,
    /// This one.
    Resume(String),
    /// A copy of this one.
    Fork(String),
}

/// Options every command that loads configuration takes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Common {
    /// `--profile`
    pub(crate) profile: Option<String>,
    /// `-c key=value`, and the dedicated flags, in order, as
    /// `(key, value, flag)`.
    pub(crate) settings: Vec<(String, String, Option<String>)>,
}

/// A run, headless or not.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Run {
    /// `-p`
    pub(crate) print: bool,
    /// The words given as the prompt, joined by spaces.
    pub(crate) prompt: Option<String>,
    pub(crate) format: Format,
    pub(crate) include_deltas: bool,
    pub(crate) choice: Choice,
    pub(crate) no_session: bool,
    pub(crate) allow: Vec<String>,
    pub(crate) deny: Vec<String>,
    pub(crate) trust_project: bool,
    pub(crate) fail_on_denial: bool,
    pub(crate) system_append: Option<String>,
    pub(crate) log_level: Option<String>,
    pub(crate) quiet: bool,
    pub(crate) verbose: bool,
    pub(crate) common: Common,
}

/// `cortex sessions …`
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SessionsCommand {
    /// `list [--all] [--limit N] [--json]`
    List {
        all: bool,
        limit: Option<u32>,
        json: bool,
    },
}

/// `cortex config …`
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ConfigCommand {
    /// `show [--origin] [--layer NAME]`
    Show {
        origin: bool,
        layer: Option<String>,
        common: Common,
    },
    /// `check`
    Check { common: Common },
    /// `defaults`
    Defaults,
}

/// `cortex permissions …`
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PermissionsCommand {
    /// `list`: this project's stored approvals, and its trust.
    List,
    /// `revoke ID…`
    Revoke { ids: Vec<String> },
    /// `trust`: trust this project, or approve the change of its rules.
    Trust,
    /// `untrust`
    Untrust,
}

/// `cortex auth …`
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AuthCommand {
    /// `status`
    Status,
    /// `login PROVIDER`: the key is read from standard input.
    Login { provider: String },
    /// `logout PROVIDER`
    Logout { provider: String },
}

/// A mistake on the command line, said to the person who typed it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct UsageError(pub(crate) String);

impl From<lexopt::Error> for UsageError {
    fn from(error: lexopt::Error) -> Self {
        Self(match error {
            lexopt::Error::MissingValue {
                option: Some(option),
            } => format!("'{option}' needs a value"),
            lexopt::Error::UnexpectedOption(option) => format!("unexpected argument '{option}'"),
            lexopt::Error::UnexpectedArgument(argument) => {
                format!("unexpected argument '{}'", argument.to_string_lossy())
            }
            lexopt::Error::NonUnicodeValue(value) => {
                format!("'{}' is not valid text", value.to_string_lossy())
            }
            other => other.to_string(),
        })
    }
}

fn unexpected(argument: &Arg<'_>) -> UsageError {
    UsageError(format!("unexpected argument '{}'", shown(argument)))
}

fn shown(argument: &Arg<'_>) -> String {
    match argument {
        Arg::Short(short) => format!("-{short}"),
        Arg::Long(long) => format!("--{long}"),
        Arg::Value(value) => value.to_string_lossy().into_owned(),
    }
}

fn text(parser: &mut Parser) -> Result<String, UsageError> {
    Ok(parser.value()?.string()?)
}

/// Reads the arguments after the program's name.
pub(crate) fn parse(arguments: Vec<OsString>) -> Result<Command, UsageError> {
    let mut parser = Parser::from_args(arguments.clone());
    if let Some(first) = arguments.first().and_then(|first| first.to_str()) {
        let topic = match first {
            "sessions" => Some(Topic::Sessions),
            "config" => Some(Topic::Config),
            "permissions" => Some(Topic::Permissions),
            "auth" => Some(Topic::Auth),
            "models" => Some(Topic::Models),
            _ => None,
        };
        if let Some(topic) = topic {
            // Past the subcommand's own name.
            let _ = parser.next()?;
            return subcommand(topic, &mut parser);
        }
    }
    run(&mut parser)
}

fn run(parser: &mut Parser) -> Result<Command, UsageError> {
    let mut run = Run::default();
    let mut words: Vec<String> = Vec::new();
    let mut choices = 0;
    let set = |run: &mut Run, key: &str, value: String, flag: &str| {
        run.common
            .settings
            .push((key.to_owned(), value, Some(flag.to_owned())));
    };
    while let Some(argument) = parser.next()? {
        match argument {
            Arg::Short('h') | Arg::Long("help") => return Ok(Command::Help(Topic::Main)),
            Arg::Short('V') | Arg::Long("version") => return Ok(Command::Version),
            Arg::Short('p') | Arg::Long("print") => {
                run.print = true;
                // `--print=TEXT`: the prompt attached to the flag.
                if let Some(attached) = parser.optional_value() {
                    words.push(attached.string()?);
                }
            }
            Arg::Long("output") => {
                run.format = match text(parser)?.as_str() {
                    "text" => Format::Text,
                    "json" => Format::Json,
                    "jsonl" => Format::Jsonl,
                    other => {
                        return Err(UsageError(format!(
                            "'{other}' is not an output format: use text, json or jsonl"
                        )));
                    }
                };
            }
            Arg::Long("include-deltas") => run.include_deltas = true,
            Arg::Long("continue") => {
                choices += 1;
                run.choice = Choice::Continue;
            }
            Arg::Long("resume") => {
                choices += 1;
                run.choice = Choice::Resume(text(parser)?);
            }
            Arg::Long("fork") => {
                choices += 1;
                run.choice = Choice::Fork(text(parser)?);
            }
            Arg::Long("no-session") => run.no_session = true,
            Arg::Long("model") => set(&mut run, "model.main", text(parser)?, "--model"),
            Arg::Long("effort") => set(&mut run, "model.effort", text(parser)?, "--effort"),
            Arg::Long("speed") => set(&mut run, "model.speed", text(parser)?, "--speed"),
            Arg::Long("permission-mode") => {
                let mode = text(parser)?;
                set(&mut run, "permissions.mode", mode, "--permission-mode");
            }
            Arg::Long("allow") => run.allow.push(text(parser)?),
            Arg::Long("deny") => run.deny.push(text(parser)?),
            Arg::Long("trust-project") => run.trust_project = true,
            Arg::Long("fail-on-denial") => run.fail_on_denial = true,
            Arg::Long("max-requests") => {
                let value = text(parser)?;
                set(&mut run, "limits.turn.requests", value, "--max-requests");
            }
            Arg::Long("max-cost") => {
                let value = text(parser)?;
                set(&mut run, "limits.turn.cost", value, "--max-cost");
            }
            Arg::Long("max-time") => {
                let value = text(parser)?;
                set(&mut run, "limits.turn.time", value, "--max-time");
            }
            Arg::Long("system-append") => run.system_append = Some(text(parser)?),
            Arg::Long("profile") => run.common.profile = Some(text(parser)?),
            Arg::Short('c') => {
                let given = text(parser)?;
                let Some((key, value)) = given.split_once('=') else {
                    return Err(UsageError(
                        "-c takes KEY=VALUE, as in -c retry.attempts=3".to_owned(),
                    ));
                };
                run.common
                    .settings
                    .push((key.trim().to_owned(), value.to_owned(), None));
            }
            Arg::Long("log-level") => run.log_level = Some(text(parser)?),
            Arg::Short('q') | Arg::Long("quiet") => run.quiet = true,
            Arg::Short('v') | Arg::Long("verbose") => run.verbose = true,
            Arg::Value(word) => words.push(word.string()?),
            other => return Err(unexpected(&other)),
        }
    }
    if choices > 1 {
        return Err(UsageError(
            "choose one of --continue, --resume and --fork".to_owned(),
        ));
    }
    if run.no_session && run.choice != Choice::New {
        return Err(UsageError(
            "--no-session starts a new session; it cannot be used with --continue, --resume \
             or --fork"
                .to_owned(),
        ));
    }
    if !words.is_empty() {
        run.prompt = Some(words.join(" "));
    }
    Ok(Command::Run(Box::new(run)))
}

fn subcommand(topic: Topic, parser: &mut Parser) -> Result<Command, UsageError> {
    if topic == Topic::Models {
        return match parser.next()? {
            None => Ok(Command::Models),
            Some(Arg::Short('h') | Arg::Long("help")) => Ok(Command::Help(topic)),
            Some(other) => Err(unexpected(&other)),
        };
    }
    let Some(first) = parser.next()? else {
        return Ok(Command::Help(topic));
    };
    let action = match first {
        Arg::Short('h') | Arg::Long("help") => return Ok(Command::Help(topic)),
        Arg::Value(action) => action.string()?,
        other => return Err(unexpected(&other)),
    };
    let wrong = |action: &str| {
        UsageError(format!(
            "'{action}' is not a subcommand of '{}'",
            match topic {
                Topic::Sessions => "sessions",
                Topic::Config => "config",
                Topic::Permissions => "permissions",
                Topic::Auth => "auth",
                Topic::Models => "models",
                Topic::Main => "",
            }
        ))
    };
    match topic {
        Topic::Sessions => match action.as_str() {
            "list" => {
                let (mut all, mut limit, mut json) = (false, None, false);
                while let Some(argument) = parser.next()? {
                    match argument {
                        Arg::Long("all") => all = true,
                        Arg::Long("json") => json = true,
                        Arg::Long("limit") => {
                            let value = text(parser)?;
                            limit = Some(value.parse().map_err(|_| {
                                UsageError(format!("'{value}' is not a whole number"))
                            })?);
                        }
                        Arg::Short('h') | Arg::Long("help") => return Ok(Command::Help(topic)),
                        other => return Err(unexpected(&other)),
                    }
                }
                Ok(Command::Sessions(SessionsCommand::List { all, limit, json }))
            }
            other => Err(wrong(other)),
        },
        Topic::Config => {
            let (mut origin, mut layer, mut common) = (false, None, Common::default());
            while let Some(argument) = parser.next()? {
                match argument {
                    Arg::Long("origin") if action == "show" => origin = true,
                    Arg::Long("layer") if action == "show" => layer = Some(text(parser)?),
                    Arg::Long("profile") if action != "defaults" => {
                        common.profile = Some(text(parser)?);
                    }
                    Arg::Short('c') if action != "defaults" => {
                        let given = text(parser)?;
                        let Some((key, value)) = given.split_once('=') else {
                            return Err(UsageError("-c takes KEY=VALUE".to_owned()));
                        };
                        common
                            .settings
                            .push((key.trim().to_owned(), value.to_owned(), None));
                    }
                    Arg::Short('h') | Arg::Long("help") => return Ok(Command::Help(topic)),
                    other => return Err(unexpected(&other)),
                }
            }
            match action.as_str() {
                "show" => Ok(Command::Config(ConfigCommand::Show {
                    origin,
                    layer,
                    common,
                })),
                "check" => Ok(Command::Config(ConfigCommand::Check { common })),
                "defaults" => Ok(Command::Config(ConfigCommand::Defaults)),
                other => Err(wrong(other)),
            }
        }
        Topic::Permissions => {
            let mut ids = Vec::new();
            while let Some(argument) = parser.next()? {
                match argument {
                    Arg::Value(id) if action == "revoke" => ids.push(id.string()?),
                    Arg::Short('h') | Arg::Long("help") => return Ok(Command::Help(topic)),
                    other => return Err(unexpected(&other)),
                }
            }
            match action.as_str() {
                "list" => Ok(Command::Permissions(PermissionsCommand::List)),
                "revoke" if ids.is_empty() => Err(UsageError(
                    "'permissions revoke' takes the ids that 'permissions list' shows".to_owned(),
                )),
                "revoke" => Ok(Command::Permissions(PermissionsCommand::Revoke { ids })),
                "trust" => Ok(Command::Permissions(PermissionsCommand::Trust)),
                "untrust" => Ok(Command::Permissions(PermissionsCommand::Untrust)),
                other => Err(wrong(other)),
            }
        }
        Topic::Auth => {
            let mut provider = None;
            while let Some(argument) = parser.next()? {
                match argument {
                    Arg::Value(name) if provider.is_none() && action != "status" => {
                        provider = Some(name.string()?);
                    }
                    Arg::Short('h') | Arg::Long("help") => return Ok(Command::Help(topic)),
                    other => return Err(unexpected(&other)),
                }
            }
            let named = |provider: Option<String>| {
                provider.ok_or_else(|| {
                    UsageError(format!("'auth {action}' takes the provider's name"))
                })
            };
            match action.as_str() {
                "status" => Ok(Command::Auth(AuthCommand::Status)),
                "login" => Ok(Command::Auth(AuthCommand::Login {
                    provider: named(provider)?,
                })),
                "logout" => Ok(Command::Auth(AuthCommand::Logout {
                    provider: named(provider)?,
                })),
                other => Err(wrong(other)),
            }
        }
        Topic::Main | Topic::Models => Err(wrong(&action)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_words(words: &[&str]) -> Result<Command, UsageError> {
        parse(words.iter().map(OsString::from).collect())
    }

    fn run_of(words: &[&str]) -> Run {
        match parse_words(words) {
            Ok(Command::Run(run)) => *run,
            other => panic!("{words:?} is not a run: {other:?}"),
        }
    }

    #[test]
    fn every_flag_in_the_help_is_one_the_parser_takes() {
        for flag in FLAGS {
            for name in flag.names.split(", ") {
                let mut words = vec![name];
                let value = match flag.value {
                    Some("FORMAT") => Some("json"),
                    Some("KEY=VALUE") => Some("retry.attempts=3"),
                    Some("RULE") => Some("exec:cargo test *"),
                    Some(_) => Some("x"),
                    None => None,
                };
                words.extend(value);
                let parsed = parse_words(&words);
                assert!(parsed.is_ok(), "{words:?}: {parsed:?}");
            }
        }
    }

    #[test]
    fn a_print_run_takes_its_prompt_from_the_words_given() {
        let run = run_of(&["-p", "explain", "the", "failure"]);
        assert!(run.print);
        assert_eq!(run.prompt.as_deref(), Some("explain the failure"));
        let run = run_of(&["--print=fix it", "--output", "jsonl"]);
        assert_eq!(run.prompt.as_deref(), Some("fix it"));
        assert_eq!(run.format, Format::Jsonl);
        let run = run_of(&["-p", "--output=json"]);
        assert_eq!(run.prompt, None);
        assert_eq!(run.format, Format::Json);
    }

    #[test]
    fn the_flags_that_choose_a_setting_become_its_key() {
        let run = run_of(&[
            "-p",
            "x",
            "--model",
            "m",
            "--effort",
            "high",
            "--speed",
            "fast",
            "--permission-mode",
            "read-only",
            "--max-requests",
            "3",
            "--max-cost",
            "0.5",
            "--max-time",
            "90s",
            "-c",
            "retry.attempts=2",
        ]);
        let keys: Vec<(&str, &str)> = run
            .common
            .settings
            .iter()
            .map(|(key, value, _)| (key.as_str(), value.as_str()))
            .collect();
        assert_eq!(
            keys,
            [
                ("model.main", "m"),
                ("model.effort", "high"),
                ("model.speed", "fast"),
                ("permissions.mode", "read-only"),
                ("limits.turn.requests", "3"),
                ("limits.turn.cost", "0.5"),
                ("limits.turn.time", "90s"),
                ("retry.attempts", "2"),
            ]
        );
        assert_eq!(run.common.settings[2].2.as_deref(), Some("--speed"));
        assert_eq!(run.common.settings[7].2, None);
    }

    #[test]
    fn mistakes_are_usage_errors() {
        for words in [
            &["--no-such-flag"][..],
            &["--output", "xml"],
            &["--continue", "--resume", "s_1"],
            &["--no-session", "--continue"],
            &["--model"],
            &["-c", "no-equals-sign"],
            &["sessions", "drop"],
            &["auth", "login"],
            &["permissions", "revoke"],
            &["config", "show", "--json"],
        ] {
            assert!(parse_words(words).is_err(), "{words:?}");
        }
        assert_eq!(
            parse_words(&["--no-such-flag"]),
            Err(UsageError("unexpected argument '--no-such-flag'".to_owned()))
        );
    }

    #[test]
    fn a_subcommand_is_chosen_by_the_first_word_only() {
        assert_eq!(
            parse_words(&["sessions", "list", "--all"]),
            Ok(Command::Sessions(SessionsCommand::List {
                all: true,
                limit: None,
                json: false
            }))
        );
        assert_eq!(
            parse_words(&["auth", "login", "anthropic"]),
            Ok(Command::Auth(AuthCommand::Login {
                provider: "anthropic".to_owned()
            }))
        );
        assert_eq!(parse_words(&["config"]), Ok(Command::Help(Topic::Config)));
        // After a flag, the same word is part of the prompt.
        assert_eq!(run_of(&["-p", "sessions"]).prompt.as_deref(), Some("sessions"));
    }
}
