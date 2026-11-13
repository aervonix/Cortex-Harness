//! The help, written by hand (decision 0011) and printed in the name the
//! program was started with: `cortex` or `cx` (decision 0013).

use std::fmt::Write as _;

use crate::args::{FLAGS, Topic};

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The help of `topic`, for a program started as `name`.
pub(crate) fn help(topic: Topic, name: &str) -> String {
    match topic {
        Topic::Main => main(name),
        Topic::Sessions => format!(
            "\
Usage: {name} sessions list [--all] [--limit N] [--json]

List the stored sessions of this workspace, newest first.

Options:
  --all        Every workspace's sessions
  --limit N    At most N sessions
  --json       One JSON object per session, a line each

Continue one with `{name} -p \"…\" --resume ID`.
"
        ),
        Topic::Config => format!(
            "\
Usage: {name} config <COMMAND>

Commands:
  show [--origin] [--layer NAME]  Print the configuration in effect here
  check                           Check every layer; exit 2 on any problem
  defaults                        Print the built-in defaults

Options:
  --origin        With show: where each value comes from
  --layer NAME    With show: one layer as it was read: default, managed,
                  user, project, project-local, profile, environment or
                  command-line
  --profile NAME  Load with the profile NAME
  -c KEY=VALUE    Set a key, as a run would
"
        ),
        Topic::Permissions => format!(
            "\
Usage: {name} permissions <COMMAND>

Commands:
  list         This project's trust, and the approvals stored for it
  revoke ID…   Remove stored approvals, by the ids that list shows
  trust        Trust this project, or approve the change of its rules
  untrust      Take this project's trust away

Trusting a project lets its configuration choose a model, allow what its
rules allow, and run what it defines. A change to its permission rules sets
them aside again until it is approved with `{name} permissions trust`.
"
        ),
        Topic::Models => format!(
            "\
Usage: {name} models

List the models of the catalog, the built-in one and the user's own, with
the provider that serves each and its context window.
"
        ),
        Topic::Auth => format!(
            "\
Usage: {name} auth <COMMAND>

Commands:
  status            Where each provider's key comes from (never the key)
  login PROVIDER    Store a key, read from standard input
  logout PROVIDER   Remove a stored key

Example:
  {name} auth login anthropic < ~/anthropic-key.txt
"
        ),
    }
}

fn main(name: &str) -> String {
    let mut text = format!(
        "\
Cortex-Harness {VERSION}: a terminal agent harness.

Usage: {name} [OPTIONS] [PROMPT]…
       {name} -p [OPTIONS] [PROMPT]…
       {name} <COMMAND>

With -p, or with a prompt when standard output is not a terminal, the
prompt is run to completion headless: the result goes to standard output
and everything else to standard error. Piped input is added before the
prompt, as context.

Commands:
  sessions      List stored sessions
  config        Show or check the configuration
  permissions   Stored approvals and project trust
  auth          Provider keys
  models        The models of the catalog

Options:
"
    );
    let width = FLAGS
        .iter()
        .map(|flag| left(flag).len())
        .max()
        .unwrap_or(0);
    for flag in FLAGS {
        let _ = writeln!(text, "  {:<width$}  {}", left(flag), flag.help);
    }
    let _ = write!(
        text,
        "
Exit codes: 0 completed, 1 internal error, 2 usage or configuration,
3 authentication, 4 provider or network, 5 the model stopped, 6 permission
denied, 7 limit reached, 8 context too large, 130 interrupted, 143
terminated.
"
    );
    text
}

fn left(flag: &crate::args::Flag) -> String {
    match flag.value {
        Some(value) => format!("{} {value}", flag.names),
        None => flag.names.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_uses_the_name_it_is_given() {
        for topic in [
            Topic::Main,
            Topic::Sessions,
            Topic::Config,
            Topic::Permissions,
            Topic::Auth,
            Topic::Models,
        ] {
            let as_cx = help(topic, "cx");
            assert!(as_cx.contains("Usage: cx "), "{topic:?}");
            assert!(!as_cx.contains("cortex "), "{topic:?}: {as_cx}");
            assert!(help(topic, "cortex").contains("Usage: cortex "), "{topic:?}");
        }
    }

    #[test]
    fn every_flag_is_in_the_help() {
        let text = help(Topic::Main, "cortex");
        for flag in FLAGS {
            assert!(text.contains(flag.names), "{}", flag.names);
        }
        assert!(!text.contains(crate::args::GUARD));
    }
}
