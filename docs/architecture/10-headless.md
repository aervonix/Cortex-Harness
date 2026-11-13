# Headless mode

Headless mode runs one or more prompts to completion without a terminal UI. It is for scripts, CI, and other programs that want a result and an exit code. It is the same core as the TUI, with a printer as the front end.

## Invocation

```
cortex -p "explain the build failure"            # prompt as an argument
git diff | cortex -p "review this"               # piped input is added to the prompt
cortex -p --output json < prompt.txt             # prompt from standard input
cortex -p "fix it" --continue                    # continue the latest session here
```

| Flag | Meaning |
|---|---|
| `-p`, `--print [PROMPT]` | Run headless. The prompt is the argument, standard input, or both (standard input first, as context). |
| `--output text\|json\|jsonl` | Output format. Default `text`. |
| `--continue`, `--resume <id>`, `--fork <id>` | Session selection, as in the TUI. |
| `--no-session` | Do not write a session log. |
| `--model`, `--effort`, `--speed` | Override the model, the reasoning level and the speed tier. They are the user's own command line, so none of them waits for the project to be trusted. `--speed` was settled in step 13a and built in step 13b ([decision 0027](../decisions/0027-speed-tiers.md)). |
| `--permission-mode <mode>` | `read-only`, `supervised`, `autonomous` or `unrestricted`. `autonomous` needs an enforcing sandbox. These are the levels in [06-permissions-and-sandbox.md](06-permissions-and-sandbox.md). |
| `--allow <rule>`, `--deny <rule>` | Add permission rules for this run, for example `--allow 'exec:cargo test *'`. |
| `--trust-project` | Treat the project as trusted for this run. |
| `--fail-on-denial` | End the run at the first call that needs an answer nobody can give, with exit code 6 ("Approvals", below). |
| `--max-requests N`, `--max-cost USD`, `--max-time DURATION` | Limits. Reaching one stops the run with exit code 7. |
| `--system-append TEXT` | Add text to the system prompt. |
| `--include-deltas` | With `jsonl`: include streaming deltas. Off by default. |
| `--profile NAME`, `-c KEY=VALUE` | Load configuration with a profile, and set any key for this run, as on any command line ([08-configuration.md](08-configuration.md)). |
| `--log-level LEVEL`, `--quiet`, `--verbose` | What the log file takes; nothing on standard error; more on standard error ("Streams", below). |

Headless mode is also selected automatically when standard output is not a terminal and a prompt was given.

## Streams

- **Standard output** carries only the result, in the chosen format. Nothing else is ever written there.
- **Standard error** carries diagnostics: warnings, retry notices, and in `text` format a brief progress line per tool call when standard error is a terminal. `--quiet` silences it. `--verbose` adds detail.
- Colour and control sequences are never written to a stream that is not a terminal.

## Output formats

### `text`

The assistant's final message, as plain text, followed by a newline. Tool activity is not printed to standard output.

### `json`

One JSON object, written when the run ends, on success and on failure alike.

```json
{
  "type": "result",
  "schema": 1,
  "session_id": "s_01JCX…",
  "outcome": "completed",
  "result": "The build fails because …",
  "stop_reason": "end",
  "usage": {"input": 18234, "cache_read": 152000, "cache_write": 2100, "output": 3412, "reasoning": 1800},
  "costs": [{"usd_nanos": 184200000, "basis": "metered"}],
  "requests": 7,
  "tool_calls": 12,
  "duration_ms": 48211,
  "files_changed": ["src/build.rs"],
  "permission_denials": []
}
```

- `outcome` is one of `completed`, `failed`, `interrupted`, `limit`, `denied`. It and the exit code always agree.
- `error`, present when the run failed, is `{ "kind", "code", "message", "request_id" }`, with `kind` from the error table in [02-runtime.md](02-runtime.md). An optional field with nothing in it is left out, as everywhere in the protocol.
- `costs` is money as the protocol writes it ([11-rpc-protocol.md](11-rpc-protocol.md), "Conventions"): whole nano-dollars with a basis, one amount per basis, left out when no price is known. `metered` is billed per token; `estimate` is what usage on a subscription would have cost. Dollars are `usd_nanos / 1e9`: `jq '[.costs[] | select(.basis == "metered") | .usd_nanos] | add / 1e9'`. (The owner chose this over a `cost_usd` float in step 13b: sums stay exact, and a metered cost is never added to an estimate.)
- `usage` is the protocol's `TokenUsage`; `reasoning` is left out when the provider does not report it.
- `files_changed` lists what the run's tools wrote, created or deleted: the editing tools' files, and what the shell tool saw a command change. A file that was only read is not in it.
- `permission_denials` lists tool calls that were refused because nothing could approve them, which is the usual reason a headless run does less than expected. Each is `{ "turn_id", "call_id", "tool", "summary", "reason" }`.
- `session_id` is left out of a run that failed before it had a session (an invalid configuration, say).

### `jsonl`

A stream of JSON objects, one per line, as they happen.

- The first line is a `session` record: `{"type": "session", "schema": 1, "session_id", "model", "workspace"}`.
- Then the same events the RPC protocol sends as `session/event` notifications, in the same shape ([11-rpc-protocol.md](11-rpc-protocol.md)). There is one event vocabulary, not two.
- By default only whole items are emitted (`item.completed`, notices, usage). `--include-deltas` adds `item.delta` events.
- The last line is a `result` record: the `json` format's object.
- **Both records are protocol types** (`HeadlessRecord` in `cortex-protocol`: `HeadlessStart` and `HeadlessResult`, an agreed addition of step 13b), in the generated schema and the TypeScript declarations ([../protocol/README.md](../protocol/README.md)). Every line of `jsonl` is told apart by its `type`, and every line is valid against the generated schema: the records as `HeadlessRecord`, the events as `SessionEvent`. The system tests check every line they read.

Every line is flushed when written, so a consumer can follow along.

## Approvals

Nothing can be asked in headless mode. A tool call that would need approval is denied, the model is told, and the call is listed in `permission_denials`.

- The run continues after a denial: the model may find another way.
- If the run ends without completing the task because of denials, the outcome is still what the model's last message makes it. Exit code 6 is used only when a denial makes the run stop (for example the model's only action was refused and it ended its turn with an error), or when `--fail-on-denial` is set, which turns the first denial into a failure: the policy then answers "stop the turn here", and the turn ends with the outcome `denied` once every call of the round has its result ([03-agent-loop.md](03-agent-loop.md), "Tool rounds").
- Pre-approve with `--allow` rules, with configuration, or with a permission mode.
- **A write of a protected path needs an answer at every mode below `unrestricted`,** `autonomous` included: git's hooks and configuration, `.cortex/`, shell start-up files, CI configuration, registry settings, and files that look like secrets ([06-permissions-and-sandbox.md](06-permissions-and-sandbox.md), "Protected paths"). In a headless run that is a denial, unless an allow rule names the path: `--allow 'write .github/workflows/**'`.
- **`--permission-mode unrestricted` asks nothing,** protected paths included, and is meant for disposable environments. Two things still hold there: a `deny` or `ask` rule the user wrote (an `ask` is a denial where nobody can be asked), and the built-in protection of the harness's own credentials, trust and approvals.

## Exit codes

One table, shared by all output formats. Pi's print and JSON modes disagree on exit status, and Opencode's run mode has only 0 and 1 (see [../reference-notes.md](../reference-notes.md)); a script should be able to branch on the code alone.

| Code | Meaning | `outcome` |
|---|---|---|
| 0 | The turn completed and the model ended normally. | `completed` |
| 1 | Internal error. A bug. | `failed` |
| 2 | Usage or configuration error. Nothing was sent to a model. | `failed` |
| 3 | Authentication error: no credentials, or the provider rejected them. | `failed` |
| 4 | Provider or network error, after retries. | `failed` |
| 5 | The model stopped without finishing: refusal, content filter, or output limit. | `failed` |
| 6 | A required permission was denied. | `denied` |
| 7 | A configured limit was reached: requests, cost or time. | `limit` |
| 8 | The context could not be made to fit. | `failed` |
| 130 | Interrupted (SIGINT, or Ctrl-C). | `interrupted` |
| 143 | Terminated (SIGTERM). | `interrupted` |

- Codes 9 to 63 are reserved for future use. Scripts should treat an unknown non-zero code as failure.
- On codes 4 to 8, 130 and 143, any text the model produced is still written, so partial work is not lost.
- On Windows, 130 and 143 are used for Ctrl-C and Ctrl-Break or a close event.

## Signals

The first SIGINT or SIGTERM cancels the turn, waits for running tools to stop, writes the final record and exits with 130 or 143. A second SIGINT (Ctrl-C) is the second interrupt, "kill now": running commands are killed at once, without the polite interrupt and the graces, and the run then ends as after the first. A third signal, or a second when either of the two is a SIGTERM, exits the process at once: what it was running is killed by the guard of process trees on Linux and by the Job Object on Windows. See [02-runtime.md](02-runtime.md).

- **On Windows** Ctrl-C is SIGINT, and Ctrl-Break and the console's close event are SIGTERM.
- **A process with no console** (started by a service or a scheduler) gets no console events, and is stopped by being ended. Its commands are started with no console either, and the run goes to its end as any other (tested on Windows).
- **The signals are watched on the main thread**, on a runtime of its own, so a core thread that is busy, or stuck, cannot keep the process from being stopped.

## What the core provides, and what the command line adds

Step 13 was built in two halves. Step 13a built the core, and everything on this page whose behaviour is the core's is there and tested in process:

| On this page | In the core (`cortex-core`, step 13a) | The command line's part (step 13b) |
|---|---|---|
| A front end that answers nothing | `ClientInfo::headless()`: a `user` client with no capabilities. With no other client, a call that needs an answer is refused. | Connect one. |
| `permission_denials` | `Core::permission_denials(session)`: each call refused because nobody could be asked, with its turn, tool, summary and reason. | Print them in the result. |
| `--fail-on-denial` | `CoreOptions::fail_on_denial`: the same refusal ends the turn with the outcome `denied`. | Set it, and map `denied` to exit code 6. |
| `--model`, `--effort`, `--speed`, `--permission-mode`, `-c` | `CoreOptions::overrides`: each as the configuration key it sets (`model.main`, `model.effort`, `model.speed`, `permissions.mode`). | Parse the flags. |
| `--max-requests`, `--max-cost`, `--max-time` | The same overrides: `limits.turn.requests`, `limits.turn.cost` and `limits.turn.time`, the limits of the one turn a run is. | Parse the flags. |
| `--system-append` | `CoreOptions::system_append`, added in step 13b: the text goes at the end of every session's system prompt. | Set it. |
| `--allow`, `--deny` | `cortex_core::parse_rule` and `CoreOptions::rules`. | Pass each flag's text. |
| `--trust-project` | `CoreOptions::trust_project`. | Set it. |
| `--no-session` | `session/new` with `ephemeral`. | Set it. |
| `--continue`, `--resume`, `--fork` | `session/list`, `session/open`, `session/fork`. | Choose the session. |
| The second signal | `turn/interrupt` with `now`: running commands are killed at once. | Count the signals, and exit. |
| Commands that do not outlive a killed harness | `CoreOptions::guard`: the program and arguments of the guard process. | Give the binary's own path and add the hidden subcommand whose body is `cortex_sandbox::tree::run_guard`. |
| Diagnostics on standard error | `cortex_base::log`: the stderr sink, off until given a level. `CoreOptions::key_found` tells the log's filter each key the core finds. | Start logging, with the sink at `warn`. |
| A spending cap | The provider's error reaches `turn.completed` as it is: kind `provider`, code `enforced_spend_limit_reached`, the provider's message. | Print it. |
| The exit code | `turn.completed`: the outcome and the error's kind. | The table above. |

## As built in step 13b

`cortex-cli` (`crates/cortex-cli/src/`). The modules are `args` (the command line, one table of flags that both the help and a test read), `help`, `headless`, `report` (the records and the formats, with no I/O, so every format is unit-tested on events written by hand), `exit`, `signals` and `commands` (the subcommands).

- **Three threads.** The core thread runs a current-thread Tokio runtime with its timer **and its I/O driver** on: the provider layer's and the process runner's waits need the first, and the HTTP client's sockets the second (step 13a's notes named only the timer; the first real request found the other). It starts the core, opens the session, starts the turn, and reads the client's queue. A writer thread writes standard output a line at a time and flushes each, through a bounded channel of 64 lines: a reader that is slow holds up the writer, never the core, whose queue merges and drops deltas for a client that is behind. The main thread watches the signals.
- **The prompt:** standard input when it is not a terminal (at most 32 MiB), then the words given, with a blank line between. A run with neither is a usage error. A script that runs `cortex -p` with a standard input it never closes should give it `</dev/null`.
- **Mode selection.** `-p`, or a prompt given while standard output is not a terminal, is headless. Anything else is the interactive application, which is step 23's: until then it says so and exits with 2.
- **What goes to standard error:** the stderr sink of the log at `warn` (`info` with `--verbose`), the session's notices (what an untrusted project held back, the sandbox's state, retries), the error of a run that failed, each denial, and in `text` format a line for each tool call when standard error is a terminal. `--quiet` turns all of it off. The log file is the state directory's `logs/`, filtered by `--log-level` or `CORTEX_LOG`. Configuration's `log.level` is not read by the log of a headless run yet ([../outstanding/log-level-from-configuration-in-headless-mode.md](../outstanding/log-level-from-configuration-in-headless-mode.md)).
- **A failure before the turn** (an invalid configuration, no key, a session that cannot be written) still writes its record in `json` and `jsonl`, with the outcome and code its kind has: `config` and `usage` 2, `auth` 3, `internal` 1.
- **Tested through the real binary** against the scripted SSE server (`crates/cortex-cli/tests/system/`): every exit code of the table (130 on Linux only: Ctrl-C cannot be sent to one process group on Windows, where the test sends Ctrl-Break, 143), every format, standard output carrying only the result, `--continue` and `--no-session`, the second Ctrl-C killing a command whose grandchild ignores SIGINT and SIGTERM at once, a harness killed outright leaving no command behind, a run with no console on Windows, and the subcommands. One run against Anthropic and one against DeepSeek, by hand: `cargo xtask live`.

## Sessions

Headless runs write session logs like any other run, so `--continue` works between invocations and a headless session can be opened in the TUI later. `--no-session` keeps everything in memory.

## What headless mode does not do

- It does not read a terminal, draw anything or wait for input other than the prompt.
- It does not load TUI code paths, so start-up is as small as the core allows. The start-up budget in [../performance-budgets.md](../performance-budgets.md) applies.
- It does not prompt for project trust. Untrusted project resources are skipped and listed on standard error.
