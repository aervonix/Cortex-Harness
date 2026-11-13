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
| `--model`, `--effort`, `--speed` | Override the model, the reasoning level and the speed tier. They are the user's own command line, so none of them waits for the project to be trusted. `--speed` was settled in step 13 ([decision 0027](../decisions/0027-speed-tiers.md)). |
| `--permission-mode <mode>` | `read-only`, `supervised`, `autonomous` or `unrestricted`. `autonomous` needs an enforcing sandbox. These are the levels in [06-permissions-and-sandbox.md](06-permissions-and-sandbox.md). |
| `--allow <rule>`, `--deny <rule>` | Add permission rules for this run, for example `--allow 'exec:cargo test *'`. |
| `--trust-project` | Treat the project as trusted for this run. |
| `--fail-on-denial` | End the run at the first call that needs an answer nobody can give, with exit code 6 ("Approvals", below). |
| `--max-requests N`, `--max-cost USD`, `--max-time DURATION` | Limits. Reaching one stops the run with exit code 7. |
| `--system-append TEXT` | Add text to the system prompt. |
| `--include-deltas` | With `jsonl`: include streaming deltas. Off by default. |

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
  "schema": 1,
  "session_id": "s_01JCX…",
  "outcome": "completed",
  "result": "The build fails because …",
  "stop_reason": "end",
  "error": null,
  "usage": {"input": 18234, "cache_read": 152000, "cache_write": 2100, "output": 3412, "reasoning": 1800},
  "cost_usd": 0.1842,
  "requests": 7,
  "tool_calls": 12,
  "duration_ms": 48211,
  "files_changed": ["src/build.rs"],
  "permission_denials": []
}
```

- `outcome` is one of `completed`, `failed`, `interrupted`, `limit`, `denied`. It and the exit code always agree.
- `error`, when present, is `{ "kind", "code", "message", "request_id" }`, with `kind` from the error table in [02-runtime.md](02-runtime.md).
- `permission_denials` lists tool calls that were refused because nothing could approve them, which is the usual reason a headless run does less than expected.

### `jsonl`

A stream of JSON objects, one per line, as they happen.

- The first line is a `session` record: schema version, session id, model, workspace.
- Then the same events the RPC protocol sends as `session/event` notifications, in the same shape ([11-rpc-protocol.md](11-rpc-protocol.md)). There is one event vocabulary, not two.
- By default only whole items are emitted (`item.completed`, notices, usage). `--include-deltas` adds `item.delta` events.
- The last line is a `result` record with the same fields as the `json` format.

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

The first SIGINT or SIGTERM cancels the turn, waits for running tools to stop, writes the final record and exits. A second signal exits at once. See [02-runtime.md](02-runtime.md).

## What the core provides, and what the command line adds

Step 13 was built in two halves. Step 13a built the core, and everything on this page whose behaviour is the core's is there and tested in process:

| On this page | In the core (`cortex-core`, step 13a) | The command line's part (step 13b) |
|---|---|---|
| A front end that answers nothing | `ClientInfo::headless()`: a `user` client with no capabilities. With no other client, a call that needs an answer is refused. | Connect one. |
| `permission_denials` | `Core::permission_denials(session)`: each call refused because nobody could be asked, with its turn, tool, summary and reason. | Print them in the result. |
| `--fail-on-denial` | `CoreOptions::fail_on_denial`: the same refusal ends the turn with the outcome `denied`. | Set it, and map `denied` to exit code 6. |
| `--model`, `--effort`, `--speed`, `--permission-mode`, `-c` | `CoreOptions::overrides`: each as the configuration key it sets (`model.main`, `model.effort`, `model.speed`, `permissions.mode`). | Parse the flags. |
| `--allow`, `--deny` | `cortex_core::parse_rule` and `CoreOptions::rules`. | Pass each flag's text. |
| `--trust-project` | `CoreOptions::trust_project`. | Set it. |
| `--no-session` | `session/new` with `ephemeral`. | Set it. |
| `--continue`, `--resume`, `--fork` | `session/list`, `session/open`, `session/fork`. | Choose the session. |
| The second signal | `turn/interrupt` with `now`: running commands are killed at once. | Count the signals, and exit. |
| Commands that do not outlive a killed harness | `CoreOptions::guard`: the program and arguments of the guard process. | Give the binary's own path and add the hidden subcommand whose body is `cortex_sandbox::tree::run_guard`. |
| Diagnostics on standard error | `cortex_base::log`: the stderr sink, off until given a level. `CoreOptions::key_found` tells the log's filter each key the core finds. | Start logging, with the sink at `warn`. |
| A spending cap | The provider's error reaches `turn.completed` as it is: kind `provider`, code `enforced_spend_limit_reached`, the provider's message. | Print it. |
| The exit code | `turn.completed`: the outcome and the error's kind. | The table above. |

## Sessions

Headless runs write session logs like any other run, so `--continue` works between invocations and a headless session can be opened in the TUI later. `--no-session` keeps everything in memory.

## What headless mode does not do

- It does not read a terminal, draw anything or wait for input other than the prompt.
- It does not load TUI code paths, so start-up is as small as the core allows. The start-up budget in [../performance-budgets.md](../performance-budgets.md) applies.
- It does not prompt for project trust. Untrusted project resources are skipped and listed on standard error.
