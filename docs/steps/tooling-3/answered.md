# Answered in the third tooling step, on 2026-11-14

This step is not a numbered step of the build plan: it built the third round of developer tooling that wave 6's sessions asked for, and fixed the nightly run of 2026-11-14. Its records are here, in a folder named for the job.

## The owner's answers

| Question | Answer |
|---|---|
| What is "a by-name break that another break in the list already covers", for the pre-flight? | A break made unable to fail by a second guard in the code, not by an overlapping break: step 13a closed the Windows credentials file by name, then its whole folder, and the break of the first could no longer fail. Only running the breaks shows it, so the pre-flight does not look for it. `mutate`'s NOT CAUGHT says, for a break whose list names its tests, that another guard may now cover it. |
| Removing the default bodies of the wrapped traits, with a `rules` check to keep them out, for item 2? | Agreed, with the `tool_call` fix. |
| A finding of the TUI design session: `cargo xtask rules` scanned a scratch folder in a worktree and failed with 3,963 findings. | Built as asked: the rules read the files git tracks; a link may still lead to a new file, and `mutate`'s copy still takes untracked files. |

## Choices made in the step

- **The nightly's cause was the colour, not the driver's recent changes.** The real-cargo test of `mutate` had never run in a scheduled nightly: the pass of 13 November (`db55572`) was on a commit with no driver. Every workflow sets `CARGO_TERM_COLOR=always`. The driver now runs cargo with `never`, takes escape sequences out all the same, and leaves cargo's progress lines out of the fallback. The real-cargo test passes with colour forced on.
- **Wrapped traits are held at compile time.** Their default bodies are removed rather than wrapped in a forwarding macro: a macro would have to be updated by whoever adds a method, which is the mistake it is meant to stop. The `rules` check is what keeps a default from coming back. A trait that gains a wrapper joins `WRAPPED`. The other traits with default bodies (`Redactor`, `ContentStore`, `CredentialStore`, `Protocol`, `Device`, `Locate`, `Rng`, `Classified`) were looked at: none is wrapped by an implementation of itself.
- **combine works in a `git clone --shared`, not a `git worktree add`.** A worktree writes into the repository: an entry under its worktrees, a branch or a detached `HEAD` of its own. A clone writes nothing there. The pull requests are fetched into the clone.
- **The A/B base is beside the head's checkout,** so that the head's `.cargo/config.toml` does not reach its build.
- **The partial-Landlock check fails unless the run named the gap.** A runner image whose kernel moves to Landlock 6 would otherwise turn the job into a copy of the blocking one, in silence.
- **The rerun helper is in the test kit.** `cortex-base`'s own test of the program search cannot use it (the test kit depends on `cortex-base`) and keeps its path made relative by hand.
- **The release start-up on Windows,** which tooling 2's `try` run measured, is recorded in [performance-budgets.md](../../performance-budgets.md), "Start-up". Its outstanding item asked about the start overhead of step 11 "in a release build", which on reading is the overhead of starting a shell command (the row "Starting a shell command"), not of starting the binary. That row still says a release build on Windows is not measured. The coordinator chose to delete the item.

## The independent reviews

A reviewer on Opus 5.5, which had not seen the session's reasoning, read the whole diff before the first push. It found nothing that leaks a secret or gives a workflow more rights. What it found, and what was done:

| Finding | Done |
|---|---|
| The bare-name rule saw only one-argument calls with a plain literal: `brief::run("git", …)`, `Command::new("git".to_owned())`, `OsStr::new("sh")`, a `let` passed it. | Fixed: the first argument whatever follows it, `brief::run` and `Pty::spawn` as starters, a literal borrowed or made into a `String`, an `OsStr` or a path, a `let` of the file. Fixture lines and breaks for each. |
| The rule lexed every Rust file, twice, in the blocking `rules` step. | Fixed: only the files whose text calls a starter, and only those that may declare a test module in a file of its own. The step takes 0.6 s on the desktop with the rules and without. |
| combine was not tested from a linked worktree, the real case. | A test: the worktree's branch, head, refs, status, stashes and the list of worktrees are the same after. |
| An A/B base inside the head's checkout gets the head's cargo configuration. | Fixed: the base is moved beside the checkout, and `ab` refuses one inside. |
| One failing test ends an A/B run. | Outstanding ([outstanding/an-ab-run-ends-at-a-failing-test.md](../../outstanding/an-ab-run-ends-at-a-failing-test.md)). |
| The head checkout kept the job's token while an A/B run builds any ref's code. | Fixed: `persist-credentials: false`. |
| A rerun of an ignored test passed the "ran something" guard. | Fixed, with a test of an ignored test. |
| A `try` filter after the command's own `--` went to the test binary. | Fixed: filters go before it. |
| The pre-flight's `cfg` exemption took any `cfg`. | Fixed: only a `cfg` of the system. |
| combine's tests start git about forty times each, which is seconds on a Windows runner. | Ignored on Windows, where the nightly's Windows job runs them; under 0.1 s each on Linux. |
| `docs/conpty.md` not in the index; step 11's record named a section this step removed; `ci.md` empty. | Fixed. |

A second review, of the changes since, found:

| Finding | Done |
|---|---|
| An A/B base named `../ab-base` was refused on Linux as inside the head: `std::path::absolute` keeps `..` there. | Fixed: compared with the dots read out, with a test and a break. |
| The base's JUnit report was not uploaded: the workflow named a file nobody wrote. | Fixed: every round's reports are kept in the head's `target/ab/`, which is uploaded. |
| The rules no longer see a new file until it is added. | Said in AGENTS.md, beside "run `cargo xtask ci`". |
| A misplaced doc comment; two docs of the `cfg` exemption. | Fixed. |
| A `let` in a test could name a program for code outside it. | Fixed, with a fixture line. |

## Found by the step's own runs

- **The breaks showed two tests that did not bite:** the pre-flight's nesting case (`in: Default::default` never met an `impl` that answers to the name), and the working directory of a rerun (a relative `ComSpec` is refused wherever the child runs). Both tests now check what they claim.
- **The dispatched nightly found the step's own workflow edit unreadable:** `run: … --ignored combine::` ends in a colon, a mapping to YAML, and GitHub read the file as no workflow. A workflow rule now refuses an unquoted value that holds `: ` or ends with `:`.
- **The partial-Landlock job built no confinement test:** they need `cortex-sandbox`'s `backends` feature, which a build of that crate alone does not turn on. The job's own check failed it; its message now names that cause beside the kernel.
