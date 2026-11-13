# 0030: Process trees are controlled from `cortex-sandbox`, and a guard process kills what a dead harness left

- **Status:** Accepted by the owner on 2026-11-13.
- **Date:** 2026-11-13

## Context

The `shell` tool runs commands a model wrote. A command must be stoppable with everything it started, and nothing of it may outlive the harness ([architecture/05-tools.md](../architecture/05-tools.md), "`shell`", "Process tree"). That takes operating-system features the standard library does not have:

| Needed | Linux | Windows |
|---|---|---|
| The command in a tree of its own | A new session, which is also a new process group and has no controlling terminal | A new process group inside a Job Object |
| A polite interrupt to the whole tree | `SIGINT` to the group | A Ctrl-Break event to the group |
| A kill of the whole tree | `SIGKILL` to the group | Terminating the Job Object |
| The tree dies with the harness | The kernel's death signal, which reaches the command itself and nothing it started | A Job Object set to kill its processes when its last handle closes |

The runner is in `cortex-tools`, where `unsafe` is denied. The rule so far allowed it in `cortex-sandbox`, `cortex-term`, and one module of `cortex-base` ([0029](0029-owner-only-files-on-windows.md)).

## Options considered

Measured on the desktop on 2026-11-13, 4 cores pinned, clean builds, by the method of [measurements/dependency-costs.md](../measurements/dependency-costs.md), with no other run active. The Windows figures are `cargo check` for the Windows target, because the desktop cannot link a Windows program. An empty program builds in 0.6 s (dev) and 1.3 s (release), and checks for Windows in 0.1 s.

| Option | Gives | Cannot give | New packages | Build |
|---|---|---|---|---|
| The standard library alone | Linux: a process group at start. Windows: the new-process-group flag, a suspended start. One pipe for merged output. | Signalling a group, Job Objects, a session, the death signal | 0 | 0 |
| `rustix` (Linux), a safe API | Signalling a group, waiting without reaping | A session, the death signal | 2 | 0.6 s, 2.2 s |
| `nix` (Linux), a safe API | The same | The same | 3 | 1.4 s, 2.6 s |
| `win32job` (Windows), a safe API | Creating a job, kill-on-close, assigning a process | Terminating a job, letting a suspended process run, Ctrl-Break | 18, with the `windows` crate and `syn` | 2.6 s to check (`windows-sys`: 0.4 s) |
| `process-wrap` (both), a safe API | A session (through its own `pre_exec`), a Job Object with a suspended start and terminate | Ctrl-Break, the death signal | 9 on Linux, 19 on Windows | 1.4 s, 3.2 s on Linux; 2.4 s to check on Windows |
| **The calls in `cortex-sandbox`, through `libc` and `windows-sys`** | Everything | | 0: both are already in the lockfile | 0.6 s, 2.0 s for a program that makes the four Linux calls |
| A new allowance for one module of `cortex-tools` | Everything | | 0 | The same |

**No crate with a safe API is enough,** whichever is chosen.

- **Windows: nothing sends Ctrl-Break safely.** `win32job` and `process-wrap` do not send it, and the Windows function is `unsafe` to call.
- **Linux: a session and the death signal must be set in the child,** between `fork` and `exec`. The standard library's safe way to start a session is not stable in the pinned toolchain (checked: `process_setsid` is refused), so it takes `pre_exec`, which is an `unsafe` function whatever crate fills its body.

So some `unsafe` of ours is needed unless the polite interrupt on Windows, or the session and the death signal on Linux, were given up. The question was where it lives.

## Decision

**The operating-system calls for process trees are in `cortex-sandbox`, in the module `tree`, behind a safe API. `cortex-tools` depends on `cortex-sandbox`. The rule about where `unsafe` may be written does not change.**

- **The module is small and each half is one file:** `tree/unix.rs` and `tree/windows.rs`. Every block has a `// SAFETY:` comment. The process is started by the standard library in both: its command line, its environment and its standard streams are built by code that is not in the module.
- **The API** (`cortex_sandbox::tree`): `Tree::spawn` starts a command as a tree; `interrupt`, `terminate` and `kill` act on all of it; `wait` blocks until the command itself has exited; `others_running` counts what it left behind; `end`, and dropping the last handle, kill what is left. `Output` is the one pipe a command writes both streams to, read by a thread that can be told to stop.
- **The dependencies:** `libc` on Unix and `windows-sys` on Windows, with the API families the module calls and no more. Both were in the build already.
- **The edge:** `cortex-tools` may depend on `cortex-sandbox` ([architecture/01-workspace.md](../architecture/01-workspace.md)). The sandbox backends (`landlock`, `seccompiler`) go behind a feature of `cortex-sandbox` when step 26 adds them, so that `cortex-tools` does not link them and `cortex-core`'s `sandbox` feature still decides.

**On Linux a guard process kills what a dead harness left.** The kernel's death signal reaches the command itself: a shell dies, and what the shell started lives on. So the harness also keeps one small guard process.

- The harness holds the writing end of a pipe to the guard, and tells it the process group of every tree it starts and of every tree that has ended.
- The guard does nothing until the pipe closes, which the kernel does when the harness is gone, however it went. Then it kills the groups it still knows, and exits.
- **It kills only what it was told.** A group it was not told of, its own group, and the ids that mean "every process" are never signalled.
- **It outlives what kills the harness.** It runs in a session of its own, so nothing aimed at the harness's terminal or process group reaches it. It ignores `SIGINT`, `SIGTERM`, `SIGHUP` and `SIGQUIT`. And its name in the process table is `cx-tree-guard`, not the harness's, so `killall cortex` takes the harness and leaves the guard to do its work, with any signal. A kill that names the guard, or that matches whole command lines (`pkill -f`), takes it too, and then only the command itself dies with the harness.
- **It is the harness's own executable** under a hidden subcommand, like the sandbox launcher, so nothing is installed. `cortex_sandbox::tree::run_guard` is its body. A guard that has died is started again, and told of every tree still running. A command whose tree cannot be put under the guard is killed again and not run.
- Both mechanisms are kept. The death signal costs one system call in a `pre_exec` that the session needs anyway, and holds if the guard cannot be started.

## Why

- **The crate that starts confined processes is the right home.** [01-workspace.md](../architecture/01-workspace.md) already gave `cortex-sandbox` Job Objects and `windows-sys`, and its Linux table already had "process group and death signal".
- **Others need the same control.** `cortex-ext` and `cortex-mcp` start hook commands and MCP servers. Both depend on `cortex-sandbox` already, and now get tree control from it without going through the shell tool.
- **The Windows sandbox will create the process there anyway.** An AppContainer process cannot be started by the standard library (step 49). With Job Objects in `cortex-tools`, that step would split one mechanism across two crates.
- **No rule changes.** A fourth place where `unsafe` may be written was the alternative, for the same code.
- **The guard is the only mechanism that reaches a grandchild on Linux** without root, a container or user namespaces. Measured on the desktop (release build, 400 runs each): telling the guard of a tree and of its end costs two pipe writes, 6 µs a command, and one process start a session. A guard process for each command would cost 105 µs a command.

## What is given up

- **`cortex-tools` now builds `cortex-sandbox`.** Today that is the tree module and nothing else. Step 26 must keep it so, with the feature described above.
- **On Linux, `pre_exec` makes the standard library fork** where it would otherwise use `posix_spawn`, and a fork copies the parent's page tables. Starting `/bin/true`: 234 µs against 188 µs from a small parent, and 761 µs against 189 µs from a parent that holds 64 MB. The budget for starting a command is 5 ms.
- **The death signal is tied to a thread.** The kernel sends it when the *thread* that started the command ends. So a command is started from the thread that polls the tool's future, the core thread, and never from the blocking pool, whose threads come and go. `Tree::spawn` says so, and the runner does it.
- **A process that leaves its group on Linux is outside the tree.** One that starts a session of its own is not signalled, not counted and not killed by the guard. The runner sees that such a process still holds the command's output, stops listening, and the result says that a process is still running and could not be stopped. Confining it is the sandbox's job ([security.md](../security.md)).
- **Waiting without reaping needs exited children to be kept,** which a process that ignores `SIGCHLD` does not get: the system reaps its children as they exit and gives their ids away. A harness can inherit that setting from whatever started it. `Tree::spawn` looks, and puts the signal back to its default, which is a change to the whole process and the one every program that waits for a child needs.
- **The guard cannot tell a group's id from a reused one** if the harness dies after a tree's command was reaped and before the guard was told. The harness tells the guard first and reaps second, so the window is one in which the harness has already died.
- **On Windows the first thread of a suspended process is found by a list of the system's threads,** because the standard library keeps that thread's handle to itself (`main_thread_handle` is not stable). It is a documented way, and costs a pass over the thread list for each command.

## Revisit when

- The standard library gains a stable way to start a session and to set the death signal without `pre_exec`. The fork then goes.
- The standard library hands out the first thread's handle on Windows. The pass over the thread list then goes.
- A Linux mechanism appears that kills a whole tree with its parent for an unprivileged process. The guard then goes.
