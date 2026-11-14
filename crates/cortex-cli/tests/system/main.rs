//! System tests (tier 3): the real `cortex` binary, run as a process.
//!
//! Test names here start with `sys_`. That prefix is what puts them in
//! nextest's process group and keeps them out of the `fast` profile.
//!
//! A test gets a [`Machine`]: a home directory and a workspace of its own,
//! under the system's temporary directory, and the scripted SSE server
//! standing in for Anthropic's endpoint. The user's configuration points
//! the built-in `anthropic` provider at the server, so the binary speaks
//! the real protocol over a real socket, and the server answers with the
//! recorded fixtures of `cortex-llm` or with a response written here.
//! Nothing of the developer's own machine is read: the binary is given an
//! environment whose home, configuration, state and data are the test's.
//!
//! | Module | What it shows |
//! |---|---|
//! | `names` | `--version`, help and usage errors under both names. |
//! | `runs` | Every exit code, each output format, and that standard output carries only the result. |
//! | `signals` | Ctrl-C, SIGTERM and Ctrl-Break; the second Ctrl-C; the guard of a killed harness; a run with no console. |
//! | `commands` | The subcommands, and a project trusted through one. |
//! | `live` | The one run against a real model, ignored: `cargo xtask live` starts it. |
//! | `tools` | The real toolbox as a strict variant declares it. |
//! | `sandbox` | The hidden subcommand `__sandbox` as the sandbox launcher, and what it adds to starting a command (Linux). |

#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stderr,
    missing_docs,
    missing_debug_implementations,
    unreachable_pub,
    reason = "a system test starts processes and prepares files, and fails by panicking; its \
              helpers are shared between the modules of this one binary"
)]

mod commands;
mod live;
mod names;
mod runs;
#[cfg(target_os = "linux")]
mod sandbox;
mod signals;
mod tools;

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use cortex_config::{Dirs, Env, Platform};
use cortex_testkit::fixture::{self, Fixture};
use cortex_testkit::{Scratch, ServerReply, SseServer};
use serde_json::{Value, json};

/// The binary Cargo built for this test run.
pub const CORTEX: &str = env!("CARGO_BIN_EXE_cortex");

/// The model of the test's configuration: one of the built-in catalog.
pub const MODEL: &str = "claude-haiku-5-5";
/// Another model of the built-in catalog.
pub const OTHER_MODEL: &str = "claude-opus-5-5";
/// The key the binary is given. The scripted server takes any.
pub const KEY: &str = "sk-ant-test-0000";

/// The name of the test that turns this binary into a child process
/// (`cortex_testkit::children`).
pub const CHILD_ENTRY: &str = "sys_child_entry";

#[test]
fn sys_child_entry() {
    cortex_testkit::children::run_if_child();
}

/// The variables that would let the developer's own set-up in.
const LEFT_OUT: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "OPENROUTER_API_KEY",
    "DEEPSEEK_API_KEY",
    "VENICE_API_KEY",
];

/// A machine of the test's own, and a scripted model.
pub struct Machine {
    pub home: PathBuf,
    pub workspace: PathBuf,
    pub server: SseServer,
    /// The variables the binary is given beside the inherited ones.
    vars: Vec<(String, OsString)>,
    _scratch: Scratch,
}

impl Machine {
    /// A machine whose user configuration names [`MODEL`] and points the
    /// `anthropic` provider at the scripted server, with `extra` after it.
    pub fn new(test: &str) -> Self {
        Self::with_config(test, "")
    }

    pub fn with_config(test: &str, extra: &str) -> Self {
        let _set_up = cortex_testkit::profile::phase("set-up");
        let scratch = Scratch::outside("cortex-cli-system", test);
        let home = scratch.path().join("home");
        let workspace = scratch.path().join("work").join("app");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&workspace).unwrap();
        let path = |parts: &[&str]| {
            let mut path = home.clone();
            path.extend(parts);
            path.into_os_string()
        };
        let vars = vec![
            ("HOME".to_owned(), home.clone().into_os_string()),
            ("USERPROFILE".to_owned(), home.clone().into_os_string()),
            ("XDG_CONFIG_HOME".to_owned(), path(&[".config"])),
            ("XDG_STATE_HOME".to_owned(), path(&[".local", "state"])),
            ("XDG_DATA_HOME".to_owned(), path(&[".local", "share"])),
            ("XDG_CACHE_HOME".to_owned(), path(&[".cache"])),
            ("APPDATA".to_owned(), path(&["AppData", "Roaming"])),
            ("LOCALAPPDATA".to_owned(), path(&["AppData", "Local"])),
            ("ANTHROPIC_API_KEY".to_owned(), OsString::from(KEY)),
        ];
        let machine = Self {
            home,
            workspace,
            server: SseServer::start(),
            vars,
            _scratch: scratch,
        };
        machine.user_config(extra);
        machine
    }

    /// Where the binary looks, worked out as it works it out.
    pub fn dirs(&self) -> Dirs {
        let mut env = Env::empty(Platform::CURRENT);
        for (name, value) in &self.vars {
            env.set(name, value.to_str().unwrap());
        }
        Dirs::from_env(&env)
    }

    /// Writes the user's configuration file: the machine's own settings as
    /// dotted keys at the top, so that `extra` may add to any table, and
    /// then `extra`.
    pub fn user_config(&self, extra: &str) {
        let file = self.dirs().user_config.unwrap();
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(
            file,
            format!(
                "session.durability = \"never\"\ncredentials.store = \"file\"\nmodel.main = \
                 \"{MODEL}\"\nretry.attempts = 1\nproviders.anthropic.base_url = \"{}\"\n{extra}",
                self.server.base_url()
            ),
        )
        .unwrap();
    }

    /// Writes the user's configuration file as it is given.
    pub fn raw_user_config(&self, content: &str) {
        fs::write(self.dirs().user_config.unwrap(), content).unwrap();
    }

    /// Writes the project's configuration file.
    pub fn project_config(&self, content: &str) {
        let dir = self.workspace.join(".cortex");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("config.toml"), content).unwrap();
    }

    /// The binary, ready to run in the workspace with the machine's
    /// environment and nothing on standard input.
    pub fn command(&self, arguments: &[&str]) -> Command {
        self.command_as(Path::new(CORTEX), arguments)
    }

    pub fn command_as(&self, program: &Path, arguments: &[&str]) -> Command {
        let mut command = Command::new(program);
        command
            .args(arguments)
            .current_dir(&self.workspace)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (name, _) in std::env::vars_os() {
            let name = name.to_string_lossy();
            if name.starts_with("CORTEX_") || LEFT_OUT.contains(&name.as_ref()) {
                command.env_remove(name.as_ref());
            }
        }
        for (name, value) in &self.vars {
            command.env(name, value);
        }
        command
    }

    /// Runs the binary to its end.
    pub fn run(&self, arguments: &[&str]) -> Run {
        let _running = cortex_testkit::profile::phase("the binary runs to its end");
        Run::of(self.command(arguments).output().expect("the binary runs"))
    }

    /// The server's next answer: a recorded Anthropic fixture of
    /// `cortex-llm`.
    pub fn reply_fixture(&self, name: &str) {
        self.reply_fixture_of("anthropic-messages", name);
    }

    /// The server's next answer: a recorded fixture of `cortex-llm`, of
    /// `protocol`.
    pub fn reply_fixture_of(&self, protocol: &str, name: &str) {
        let directory = fixture::directory(
            concat!(env!("CARGO_MANIFEST_DIR"), "/../cortex-llm"),
            protocol,
        )
        .join(name);
        let recorded = Fixture::load(&directory).unwrap();
        let mut reply = ServerReply::status(recorded.status);
        for (header, value) in recorded.headers.iter() {
            reply = reply.header(header, value);
        }
        self.server.push(reply.send(recorded.response));
    }

    /// The server's next answer: the model says `text` and ends its turn.
    pub fn reply_text(&self, text: &str) {
        self.server.push(ServerReply::sse().send(sse(&[
            message_start(),
            json!({"type": "content_block_start", "index": 0,
                "content_block": {"type": "text", "text": ""}}),
            json!({"type": "content_block_delta", "index": 0,
                "delta": {"type": "text_delta", "text": text}}),
            json!({"type": "content_block_stop", "index": 0}),
            message_delta("end_turn"),
            json!({"type": "message_stop"}),
        ])));
    }

    /// The server's next answer: the model calls `tool` with `input`.
    pub fn reply_tool(&self, id: &str, tool: &str, input: &Value) {
        self.server.push(ServerReply::sse().send(sse(&[
            message_start(),
            json!({"type": "content_block_start", "index": 0,
                "content_block": {"type": "tool_use", "id": id, "name": tool, "input": {}}}),
            json!({"type": "content_block_delta", "index": 0,
                "delta": {"type": "input_json_delta", "partial_json": input.to_string()}}),
            json!({"type": "content_block_stop", "index": 0}),
            message_delta("tool_use"),
            json!({"type": "message_stop"}),
        ])));
    }
}

fn message_start() -> Value {
    json!({"type": "message_start", "message": {"model": MODEL, "id": "msg_test",
        "type": "message", "role": "assistant", "content": [], "stop_reason": null,
        "usage": {"input_tokens": 20, "cache_creation_input_tokens": 0,
            "cache_read_input_tokens": 0, "output_tokens": 1}}})
}

fn message_delta(stop: &str) -> Value {
    json!({"type": "message_delta", "delta": {"stop_reason": stop},
        "usage": {"output_tokens": 12}})
}

/// Events as an Anthropic stream writes them.
fn sse(events: &[Value]) -> Vec<u8> {
    let mut body = String::new();
    for event in events {
        body.push_str(&format!(
            "event: {}\ndata: {event}\n\n",
            event["type"].as_str().unwrap()
        ));
    }
    body.into_bytes()
}

/// What a finished run left.
#[derive(Debug)]
pub struct Run {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl Run {
    pub fn of(output: Output) -> Self {
        Self {
            code: output.status.code(),
            stdout: String::from_utf8(output.stdout).expect("standard output is UTF-8"),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    /// Asserts the exit code, showing both streams when it is not.
    pub fn expect_code(&self, code: i32) -> &Self {
        assert_eq!(
            self.code,
            Some(code),
            "stdout:\n{}\nstderr:\n{}",
            self.stdout,
            self.stderr
        );
        self
    }

    /// Standard output as the one JSON object of `--output json`.
    pub fn json(&self) -> Value {
        assert_eq!(
            self.stdout.matches('\n').count(),
            1,
            "json is one line: {}",
            self.stdout
        );
        let value: Value = serde_json::from_str(&self.stdout).expect("standard output is JSON");
        schema_check("HeadlessRecord", &value);
        value
    }
}

/// The generated schema of the protocol, as checked in.
fn schema() -> &'static Value {
    static SCHEMA: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(|| {
        let file = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/protocol/schema/cortex-protocol.schema.json");
        serde_json::from_str(&fs::read_to_string(file).unwrap()).unwrap()
    })
}

/// Fails unless `value` fits the definition `name` of the generated schema.
pub fn schema_check(name: &str, value: &Value) {
    if let Err(problem) = cortex_testkit::schema::check(schema(), name, value) {
        panic!("not a valid {name}: {problem}\n  in {value}");
    }
}

/// Every line of `--output jsonl`, each checked against the generated
/// schema: the first is a `session` record, the last a `result` record, and
/// every one between is an event as `session/event` carries it.
pub fn jsonl(stdout: &str) -> Vec<Value> {
    assert!(stdout.ends_with('\n'), "every line ends: {stdout}");
    let lines: Vec<Value> = stdout
        .lines()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|error| panic!("{error}: {line}")))
        .collect();
    assert!(lines.len() >= 2, "{stdout}");
    let last = lines.len() - 1;
    for (index, line) in lines.iter().enumerate() {
        if index == 0 || index == last {
            schema_check("HeadlessRecord", line);
        } else {
            schema_check("SessionEvent", line);
        }
    }
    assert_eq!(lines[0]["type"], "session");
    assert_eq!(lines[last]["type"], "result");
    lines
}

/// Prints a run's streams when a test that started it fails later.
pub fn shown(run: &Run) -> String {
    format!("stdout:\n{}\nstderr:\n{}", run.stdout, run.stderr)
}
