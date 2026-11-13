# After step 13b: the command line and headless mode

Step 13b built `cortex-cli`: the command line, headless mode with its three output formats and its exit codes, the subcommands, signals and the hidden guard subcommand, with 21 unit tests and 24 system tests through the real binary. It added the subcommands' functions to `cortex-core` (`manage`), the reduction of tools for a strict variant to `cortex-llm` (`strict`), the headless records to `cortex-protocol`, the schema validator to `cortex-testkit`, and `cargo xtask startup`. It adds one direct dependency, `lexopt`, and two packages to a Unix build through Tokio's `signal` (`signal-hook-registry`, `errno`).

## At a desk

The desktop, through `rx`; another session shares it, and whether it was building at the time is not known.

| | |
|---|---|
| `cargo xtask ci`, tier 1 (unit) | 7.5 s of 20 s |
| `cargo xtask ci`, tier 2 (integration) | 19.7 s of 40 s |
| `cargo xtask ci`, tier 3 (system) | 3.7 s of 60 s |
| `cortex-cli`'s own tests | 0.5 s of test time: 21 unit tests, 24 system tests |

## Start-up, release build

`cargo xtask startup target/release/cortex --budget desktop`: 30 runs each after one that is not counted, the binary under its own name and under `cx`, a hard link beside it.

| What | Median | 95th percentile | Budget |
|---|---:|---:|---:|
| `cortex --version` | 0.56 ms | 0.71 ms | 5 ms |
| `cx --version` | 0.54 ms | 0.62 ms | 5 ms |
| headless `cortex -p`, to its request | 12.77 ms | 13.47 ms | 15 ms |
| headless `cx -p`, to its request | 12.74 ms | 13.70 ms | 15 ms |

## Breaks

`cargo xtask mutate step-13b` (`mutations/step-13b.breaks`): 29 breaks, 22 written with the code and 7 with the tests the independent review asked for. Each is caught by the tests its list names. One, `strict-open-object-kept`, is caught by a unit test alone: the one open object in today's toolbox is in a tool that Anthropic's limit on optional parameters already keeps out of strict mode, so no request shows the difference.

## The live runs

| Run | Result | Spend |
|---|---|---|
| `cargo xtask live` (Anthropic), first | The smoke test passed; the headless run was refused by Anthropic: `maxItems` in a strict tool | $0.00036, and nothing for the refused request |
| The headless run, twice more while it was fixed | Refused: an object left open; 25 optional parameters of a limit of 24 | nothing |
| The headless run, fixed | Completed: "forty-two", 2 requests, 1 tool call | $0.00063 |
| `cargo xtask live deepseek` | The smoke test, and the headless run with the whole toolbox: completed | $0.00089 and $0.00113 |
| `cargo xtask live` (Anthropic), last | Both passed | $0.00020 and $0.00012 |
| **All of them** | | **about $0.004** |

## The independent review

One review, on Opus, of the whole diff against the spec, before the first push. It found no high-severity bug. What it found, and what became of it:

| Finding | Severity | What was done |
|---|---|---|
| `--deny`, `--trust-project`, `--resume`, `--fork` and `--quiet` had no test; the second Ctrl-C's test would pass for a binary that exits on it | medium | Tests for each, and the second Ctrl-C's run must write its record; five breaks |
| Signals were dropped once the core thread was done, so a reader that stopped reading held the process; and not taken while the writer waited for one | medium-low | Signals are watched until the last line is written, and any signal exits once the core is done; a blocked write still takes them and interrupts the turn. Not tested: [outstanding](../../outstanding/headless-details-left-open.md) |
| The strict reduction removed a fixed list of keywords, so an MCP tool with `oneOf` would be sent as strict and refused | medium-low | An allow-list: a schema with any keyword its variant is not known to take, a list of schemas for `items`, or recursion is sent whole and not strict; OpenAI's root `anyOf` too. Unit tests and a break |
| Standard input that is not UTF-8 failed the run | low-medium | Read with U+FFFD; a test and a break |
| Trust cannot be approved for a profile a project defines | low-medium | [Outstanding](../../outstanding/trust-of-a-profile-a-project-defines.md) |
| The console's close event may end the process before its record | low | Said in the headless page; [outstanding](../../outstanding/headless-details-left-open.md) |
| A repository's rules and a model's arguments reached the terminal with their control characters | low | Escaped everywhere text reaches the terminal; a unit test and a break |
| The runtime page's "within two seconds" and "cancel the session token"; `--quiet` printed a bad log level; no record on a core-thread panic | low | The pages say what is built; `--quiet` silences it; a panic writes an `internal` record. A command line that cannot be read writes none, and the page says so |
| Subcommands panicked on a closed pipe; `auth login` took any provider name and cut a long key silently | low | Output that ends quietly; an unknown provider and an over-long key are refused; a test and a break |
| Early notices lost on a failure before the turn; an interrupted record's `error` differs by timing; `files_changed` trusts any tool's payload; `--continue` takes the newest created | low | [Outstanding](../../outstanding/headless-details-left-open.md), and [one for `--continue`](../../outstanding/continue-chooses-the-newest-session.md) |
