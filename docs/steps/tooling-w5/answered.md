# Answered in the wave 5 tooling step, on 2026-11-13

This step is not a numbered step of the build plan: it built the developer tooling the wave 4 sessions asked for. Its records are here, in a folder named for the job.

## Decided with the owner

- **The order of events in the loop's tests is seeded through `run_blocking`, behind a cargo feature.** The varying order was not the loop picking at random: it looks at what is ready in a fixed order. It was the blocking pool, where each call is planned and each result bounded. The seam is `cortex_tools::InlineBlocking`, behind the feature `test-hooks` of `cortex-tools`, which only a dev-dependency may turn on. `cargo xtask rules` checks that. What a workspace test build does with the feature is in [testing-and-ci.md](../../testing-and-ci.md), "A seeded order of events".
- **"Every break still applies" is a blocking check.** `cargo xtask check` has a `breaks` step. A change that moves code out from under a break updates the break in the same pull request, or deletes it. The message names the list and the line and says which.
- **The break lists are `mutations/<name>.breaks`,** one file for each step or crate, in a line format of our own that `xtask` reads. Not TOML: `xtask` is built before every CI job and has no TOML parser, and a parser there costs every job its build.
- **`cargo xtask mutate` stops starting work after five minutes and carries on the next time,** so that a long list gives a shared build machine back between slices.
- **Two checks the docs split asked for were added to `cargo xtask rules`:** every link inside the repository leads to a file and a heading that are there, and the step records and open items are where [the layout](../README.md) puts them. `cargo xtask links` runs the two alone. On the tree as it was, every Markdown link and anchor already held.
- **This folder is named for the job,** `tooling-w5`, because the work is not a numbered step. [The layout](../README.md) says how such a folder is named.

## What running the seeded lists showed

The 101 breaks of steps 7, 8, 9, 10, 12 and 14 were written again from the pull requests' descriptions, the code's own documentation and the tests, and then run. Every one is caught. Four entries named a test that passes with the break made, and were corrected to the tests that fail:

| Break | The test that was named and passes | What catches it |
|---|---|---|
| `step-07/questions-asked-together` | `rounds::questions_are_asked_one_at_a_time_in_call_order_while_allowed_calls_run` | `denials::a_stop_leaves_no_question_open_and_puts_no_other` |
| `step-07/cancellation-not-seen-in-wake` | `interrupt::interrupted_while_text_is_streaming_keeps_the_text_marked_interrupted`, `interrupt::interrupted_right_as_the_response_starts` | `transitions::an_interruption_is_taken_in_every_state_that_waits` |
| `step-08/long-line-cut-inside-a-character` | `store::a_long_line_arriving_in_pieces_is_still_redacted` | `store::tests::a_long_line_is_cut_only_between_characters` |
| `step-09/untrusted-project-sets-what-needs-trust` | `trust::trust_turns_on_what_needs_trust_and_never_what_is_the_users_alone` | `trust::an_untrusted_project_gets_its_preferences_and_nothing_that_could_hurt` and ten others |

**The first row is a finding about a test, not about the list.** `rounds::questions_are_asked_one_at_a_time_in_call_order_while_allowed_calls_run` is named for the rule "no other question is put while one is open", and passes with that rule removed from `Round::ask_next`. Its two questions are answered at once, so two open together look the same to it as one after the other. The rule is held by one test of step 12. The test is step 7's, and this step did not change it.

**This step's own list found a flaw in the driver.** `mutations/tooling-w5.breaks` has 23 breaks of the step's own code. Two of them are of the seeded order in `cortex-tools`, whose tests are built only when `cortex-agent`'s tests are in the same build. They were reported as naming tests that do not exist: the tests with nothing broken had been run for each crate by itself, where those tests are not built. They are now run for each set of packages a break runs, and every break does what its list expects.

**Step 31's own breaks were converted, and all of them still apply.** The session of step 31 kept its 87 breaks as a list of text before and after, with the tests each one failed. A script turned the list into `mutations/step-31.breaks`: the break's own name as its `rule`, the first two tests that failed as its `caught-by`. Two breaks named code that is in the file twice once layout is not counted, and were given the function they are in. All 87 apply and are caught, six of which the session's exact-text driver could no longer apply in its last run. With them `mutations/` holds 211 breaks.

The lists of the steps before it are a reconstruction. They hold the rules the descriptions and the tests name, not the 300 or so breaks the sessions made at the time, which were not kept.

## The seeded order, and the tests that were there

Every test of `cortex-agent` passed under seeds 0 to 299 when the rig was seeded, and again under 1000 to 2999 before this step was reported. No test depended on the order.

## Tests that still have a scratch directory of their own

`cortex_testkit::Scratch` replaced the copies in the integration tests of `cortex-config`, `cortex-policy`, `cortex-session` and `cortex-agent`, where the move was a change of name. These were left:

| Where | Why it was left |
|---|---|
| `crates/cortex-base/tests/it/main.rs` (`Scratch`, 87 uses) | `cortex-base` has no dev-dependency on the test kit, which depends on it. Its helper also makes path resolvers. Adding the edge is a decision for the crate's owner. |
| `crates/cortex-tools/tests/it/main.rs` (`Workspace`) and `tests/it/changes.rs` (its own `git`) | The workspace is more than a directory (a path resolver, a file table, checkpoints), and step 13a was changing these files at the same time. |
| `crates/cortex-core/tests/it/undo.rs` (`World`) | The same, and it is step 13a's crate. |
| `crates/cortex-cli/tests/system/main.rs`, `crates/cortex-sandbox/tests/it/tree.rs` | One directory each, made in one line. |
| Unit tests that use the system's temporary directory with the process id in the name: `cortex-session` (`lock.rs`, `store.rs`, `session.rs`), `cortex-config` (`dirs.rs`, `credentials/file.rs`), `cortex-tools` (`change.rs`, `read.rs`), `cortex-base` (`atomic.rs`, `nofollow.rs`) | Each is a few lines with its own clean-up, and some need a directory another test must not share. `Scratch::outside` is the replacement when one of them is next changed. |

## What the independent review found

A reviewer that had seen none of the work read the whole diff before the first push. It found no security hole. What it found, and what was done, each fix with a test:

| Finding | Fix |
|---|---|
| `mutate --again` could not be carried on after the time ran out: the same command forgot the results again | The command says to carry on without `--again` |
| `stress` would report a test that passed only on its retry as never failing | Such a repeat counts as failed |
| A run that failed without a verdict was kept for good, and its message named a log the next call wrote over | It is tried again by the next run, and each failure keeps a log of its own |
| Two runs on one copy would undo each other's breaks | One run at a time: the second is refused while the first holds the copy |
| `ci --tiers` read the last run's report when this run's build failed | The old report is removed first |
| `windows-try` refused several crates: the split was at a two-character string, not at either character | Split at spaces and commas, tried in PowerShell 7; names are matched with case |
| The `test-hooks` rule missed a default feature and a feature that passes it on | Both are refused |
| `(x,)` and `(x)` were the same code to the check that a break changes something; `t.0.1` was read as one number | Commas count in that check; a number after a `.` is a field's name |
| A scratch directory named `""`, `.` or a path would remove another test's; its git could be pointed elsewhere by the environment | Such names are refused; the variables are dropped |
| A break's `file` could be a Windows drive path | Refused |
| A test case with no time was dropped, its failure with it | It is counted, with no time |
| Tests would not have caught: a copy that rewrote every file, a break not put back after an error, the wrong exit status | A test for each |

**The review was run twice.** The first reviewer ran on a model that was not chosen, so a second ran on Opus over the diff as it stood after the first's fixes. It found no security hole either, and these, each fixed with a test:

| Finding | Fix |
|---|---|
| Inside the copy, on a checkout, `xtask`'s test that the repository follows its rules checked nothing: git, asked in a directory it ignores, lists no file. A break of a rule that only that test catches would have been reported as not caught, there and not on a machine with no repository | Git is asked for the files only at the top of a checkout; anywhere else the directory is walked |
| A search could begin or end inside an operator: `= 0` found the second half of `x == 0` | A match that splits an operator of two characters is no match |
| The link rule took a footnote for a link, read links inside HTML comments, took a code span that begins a line for a fence, numbered a third heading of one name wrongly when a heading already had the numbered name, and read no comment that follows code | Each corrected |
| On Windows a scratch directory named `x.` or `...` is another directory | Names that end in a dot or a space are refused |
| A break's tests inherited the caller's seed, property scale and update switches | They are taken out of its environment |
| `--minutes 1e300` panicked; `stress` did not read `--package=x` or `-px`, and read a `-p` after `--` | Corrected |
| Tests would not have caught: the wrong seed handed to a stress run, a build that takes the caller's target directory, package sets compared in the order they were written, a link followed while a directory is removed | A test for each |

Left as it is, and written down in [testing-and-ci.md](../../testing-and-ci.md), "What it cannot tell" and "A seeded order of events": a break in code the build does not compile is reported as not caught; work that waits for another task of its own runtime never finishes under the seeded order; "caught" with no test named is one failing test in one run; the time budget bounds when work starts, not how long the first build takes.

## Not built

- **`cargo xtask api-index`,** the one-page index of each crate's public items. It was the last item, "if time allows".
- **`rx`** needed no change: `rx test -p <crate>`, `rx stress …` and `rx mutate …` reach the new commands as they are.
