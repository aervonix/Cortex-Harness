# Answered in the second tooling step, on 2026-11-13

This step is not a numbered step of the build plan: it built the second round of developer tooling that wave 5's sessions asked for. Its records are here, in a folder named for the job.

## Choices made in the step

- **The timeout report runs inside the test process, from the test kit,** and not from nextest. nextest 0.9.151 has no hook before it stops a test: its configuration refuses the experimental `pre-timeout-scripts`. A wrapper process around every test would cost every passing test a process start (about 1 ms on Linux, several on Windows, over some 2,000 tests). So the report covers the tests that use the test kit's waits, which are the tests that hang: process tests, socket tests, the scripted server. What is left out is an open item ([outstanding](../../outstanding/timeout-report-for-tests-without-the-test-kit.md)).
- **A gate is named by where it was made,** with `#[track_caller]` on `Gate::new`, so that no test has to name its gates for the report to say which one a thread waits on.
- **The tests of the report and of the race pause are ignored in the blocking checks,** because they wait on purpose: the report's test waits most of a second for it, and on Windows asks PowerShell for the process tree. The nightly workflow runs both, on Linux and in its Windows release job.
- **`try` decides the command in Rust, not in the workflow's script.** The workflow passes each input as an argument through the environment; `cargo xtask try` checks the words and maps them, under unit tests. A new repo rule refuses any workflow whose `run:` script has an input or a part of the event pasted into its text. Every workflow there was already followed it.
- **`windows-try` is kept as it is,** inputs and all, until the coordinator says the running sessions have moved to `try`. It is not made a wrapper of `try`: a wrapper would change its log and its artifact's name under sessions that read them.
- **`stress --cores` pins the command itself,** and everything it starts inherits the pin: `taskset --all-tasks` on Linux, the process's affinity mask through PowerShell on Windows. The busy loops are threads of the command, so they die with it. Neither needs a dependency in `xtask`, which every CI job builds first.
- **`mutate --jobs` jobs are threads of one command,** each with a copy of the tree and a build directory of its own. Under `rx` they share the one test lock the command holds, and `--minutes` budgets them together: the lock is held no longer than one job's run with the same budget.
- **`decide` runs a probe that is an ignored test of `cortex-policy`'s integration binary.** Building the engine into `xtask` would add `cortex-policy` and its crates to every CI job's first build; a new example binary would be one more link step in the blocking build.
- **A change to the toolbox is a warning, not a failure.** The tools the binary sends each provider are kept with the toolbox fixtures; a system test compares and, on a difference, writes a warning to the job's summary page. A failure would make every change to a tool's description wait for a live recording, which needs keys no CI job and few sessions have. A change to the builder or the strict reduction, for the kept tools, is a failure: the recorded request is compared byte for byte.
- **The spend ledger is the user's, outside the repository:** `~/.local/state/cortex-live/spend.jsonl` (or `%LOCALAPPDATA%`, or `CORTEX_LIVE_LEDGER`). On the desktop that `rx` runs on, it is the desktop's.

## Not built

- **`cargo xtask api-index`** (item 10, "if time allows") was not built.

## The shell guard

[shell-guard.md](../../shell-guard.md) is what the step found by trying. Beside the false positives listed there, four commands with no git in them at all were refused as "too complex to verify", each a heredoc in a longer command: `mkdir -p <dir> && cat > <file> <<'EOF'` … `EOF`, a second `cat > <file> <<'EOF'` … `EOF`, then `ls <dir> | wc -l`; a `set -e; d=…; mkdir -p …; cd $d` followed by four `cat > file <<'EOF'` heredocs and a `cargo` command; a `cd <worktree> && cat > <file> <<'PYEOF'` … `PYEOF` followed by `echo`; and a `cd <worktree> && python3 - <<'EOF'` … `EOF` followed by `PATH=…:$PATH cargo …`. The same commands split into one heredoc per call were accepted.
