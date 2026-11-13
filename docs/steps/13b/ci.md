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

`cargo xtask mutate step-13b`: 22 breaks, each caught by the tests its list names (`mutations/step-13b.breaks`).

## The live runs

| Run | Result | Spend |
|---|---|---|
| `cargo xtask live` (Anthropic), first | The smoke test passed; the headless run was refused by Anthropic: `maxItems` in a strict tool | $0.00036, and nothing for the refused request |
| The headless run, twice more while it was fixed | Refused: an object left open; 25 optional parameters of a limit of 24 | nothing |
| The headless run, fixed | Completed: "forty-two", 2 requests, 1 tool call | $0.00063 |
| `cargo xtask live deepseek` | The smoke test, and the headless run with the whole toolbox: completed | $0.00089 and $0.00113 |
| `cargo xtask live` (Anthropic), last | Both passed | $0.00020 and $0.00012 |
| **All of them** | | **about $0.004** |
