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
| `--model`, `--effort` | Override the model and reasoning level. |
| `--permission-mode <mode>` | `read-only`, `supervised`, `autonomous` or `unrestricted`. `autonomous` needs an enforcing sandbox. These are the levels in [06-permissions-and-sandbox.md](06-permissions-and-sandbox.md). |
| `--allow <rule>`, `--deny <rule>` | Add permission rules for this run, for example `--allow 'exec:cargo test *'`. |
| `--trust-project` | Treat the project as trusted for this run. |
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
- Pre-approve with `--allow` rules, with configuration, or with a permission mode. `--permission-mode unrestricted` approves everything and is meant for disposable environments.

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

## Sessions

Headless runs write session logs like any other run, so `--continue` works between invocations and a headless session can be opened in the TUI later. `--no-session` keeps everything in memory.

## What headless mode does not do

- It does not read a terminal, draw anything or wait for input other than the prompt.
- It does not load TUI code paths, so start-up is as small as the core allows. The start-up budget in [../performance-budgets.md](../performance-budgets.md) applies.
- It does not prompt for project trust. Untrusted project resources are skipped and listed on standard error.
