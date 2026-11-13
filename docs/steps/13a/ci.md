# After step 13a: the core

Step 13a built `cortex-core` (about 7,000 lines of source with their unit tests, and 5,000 of integration and system tests: 29 unit and 64 integration and system tests), and added to four other crates what it needed: the logging subscriber, the secret filter, the `Timer` trait and git's flags to `cortex-base`; the second "kill now" token and the shell command's environment to `cortex-tools` and `cortex-agent`; the mark on an altered entry and the `plan` entry to `cortex-session`; one method to `cortex-policy`. It adds two direct dependencies, `tracing` and `tracing-core`, which every build already had, and no package to the workspace.

## Measured at a desk, before the pull request

On the desktop, in cargo's `ci` profile, with the whole workspace (`cargo xtask ci`), on main with steps 12, 14 and 31 and the tooling step in it:

| Tier | Tests | Time | Budget | Used |
|---|---|---|---|---|
| 1, unit | 918 | 6.3 s | 20 s | 32% |
| 2, integration | 877 | 20.3 s | 40 s | 51% |
| 3, system | 85 | 2.6 s | 60 s | 4% |

- **What the core's own tests cost,** in the same profile: 0.9 s for its unit tests, 1.5 s for its integration tests and 0.4 s for the ones that start a shell or git.
- **The core's tests were first three times as slow.** Each session asked its shell for its version (a process), and each session's log was flushed to the disk at every turn. The core now asks a shell its version once in its life, and the tests' configuration sets `session.durability = "never"`: 4.3 s became 1.5 s. On Windows, where starting PowerShell takes a second, the first of the two would have been most of a tier's budget.
- **Masking secrets** in a 64 MiB build log of 645,277 lines, with a key in one line of a thousand: 142 ms, which is 451 MiB a second and 220 ns a line (release build, 28 threads, other sessions' builds running at the time). The filter was 216 MiB a second before it looked at a key format's second byte ([performance-budgets.md](../../performance-budgets.md), "Data paths").

## That the tests bite

- **The tests were run many times over first.** `cargo xtask stress -p cortex-agent -p cortex-core --count 50`: 273 tests, seeds 0 to 49, every test passed every time. Before the rig was seeded the core's tests were run 200 times over twice, with no failure.
- **94 rules were each broken on purpose,** one at a time ([`mutations/step-13a.breaks`](../../../mutations/step-13a.breaks)): each seam between the core and the crate whose trait it fills, and each finding of the review. The first pass over the first 58 caught 53. Four were not caught, and showed four tests that could not fail:
  - a walk of a tree was never shown to the engine as a read of a tree, and no test asked for one outside the workspace;
  - an untrusted project told to the engine as trusted changed nothing a test saw, because configuration had already held the project's rules back;
  - the test of the protected configuration directory used a client that answers nothing, so the read was refused for want of anyone to ask, protected or not (two breaks).
  Each got a test, or a test that could fail, and all 94 are caught now.
- **An independent review** (a reviewer that had not seen the step's reasoning, on another model) read the whole diff before the first push. It found 2 high, 7 medium and 8 low. The high ones: an `observer` could rewind a session, fork one, start one and shut the core down; and a key in a known format was missed in a provider's raw block when a written-out line break stood before it, so the block was written with the key in it. Every confirmed finding was fixed with a test, except two whose case cannot be made in a test: an effect the engine does not know (there is none to make), and a question withdrawn in the instant it is answered.

## What is not tested on Windows

- **The second interrupt through the core** kills a command that ignores the first: the test is Unix-only, because it needs a command that ignores an interrupt. The runner's own tests of the second token run on both.
- **A workspace behind a link** (`PWD`), **a configuration directory behind a link**, and **a credential store that cannot be opened** are tested on Unix only: the first two make a symbolic link, and Windows has a keychain.
- **The core could not be linted for Windows at a desk** (its TLS library needs a Windows C compiler): CI is the first build of it there.
