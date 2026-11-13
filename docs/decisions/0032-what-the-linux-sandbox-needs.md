# 0032: What the Linux sandbox needs of the kernel, and what it lets a command reach

- **Status:** Accepted. Decided in step 26, on 2026-11-13, and checked with the coordinator, who speaks for the owner in the build steps.
- **Date:** 2026-11-13

## Context

[Decision 0009](0009-sandboxing.md) chose Landlock and seccomp, applied by a re-executed launcher, and said that a kernel without Landlock leaves commands unconfined and `autonomous` refused. Three questions were left for the step that builds it.

1. **A partial Landlock.** Landlock gains rights version by version. A kernel can have Landlock and lack a right the policy needs. Does it count as enforcing?
2. **The network.** Neither mechanism can filter by host. What exactly may a command with the network off reach?
3. **Failing closed.** Once the harness has said the sandbox enforces, what happens when applying it fails?

The kernels the project runs on, measured on 2026-11-13: the hosted `ubuntu-24.04` runners (x86-64 and arm64) have Linux 7.0 and Landlock 8; `ubuntu-22.04` has Linux 6.8 and Landlock 4; the desktop's WSL 2 has Linux 6.18 and Landlock 7. Ubuntu 24.04's own kernel is 6.8 (Landlock 4); Debian 12's is 6.1 (Landlock 2).

| Landlock | Linux | What it adds that matters here |
|---|---|---|
| 1 | 5.13 | Reading, writing, making and removing files and directories. A rename or a link from one directory to another is always refused. |
| 2 | 5.19 | `REFER`: such a rename or link, between trees that allow it. |
| 3 | 6.2 | `TRUNCATE`: emptying a file with `truncate()`. Before it, a sandboxed command could empty any file the user can write. |
| 4 | 6.7 | TCP port rules. Not used: seccomp settles the network. |
| 5 | 6.10 | `ioctl` on device files. Not used. |
| 6 | 6.12 | Scopes: a command's signals, and its connections to abstract Unix sockets, stay inside the sandbox. |
| 7 | 6.15 | Logging of denials to the audit log. Not used. |
| 8 | 7.0 | Restricting every thread at once. Not needed: the launcher has one thread. |
| 9 | 7.1 | Connecting to a Unix socket by its path. Not used: seccomp settles it on every kernel alike. |

## Options considered

**A partial Landlock.**

| Option | Assessment |
|---|---|
| Enforcing from version 1 | Version 1 and 2 let a command empty any file the user can write (`truncate -s 0 ~/.bashrc`): writes outside the workspace do not all fail. Version 1 also breaks git, which renames across directories. |
| Enforcing from version 3, with what is missing named | Every write outside the write roots fails. What 3 to 5 lack is scoping of signals: a command can stop or kill the user's other processes, the harness among them. That is disruption, not a way out: it reads nothing and writes nothing new. Ubuntu 24.04's kernel (Landlock 4) is in this band. |
| Enforcing from version 6 only | No gap. But `autonomous` would be refused on Ubuntu 24.04's own kernel and every kernel before 6.12, for a gap that is not a confinement of files or the network. |
| Below the minimum, apply what there is and report "off" | A command the user approved as unconfined would then fail on what the partial sandbox does refuse (the network). Two states a user cannot tell apart from the status line. |

**The network with `sandbox.network = "off"`.**

| Option | Assessment |
|---|---|
| Refuse internet sockets only (`AF_INET`, `AF_INET6`) | Leaves Unix sockets: the user's D-Bus session bus (`systemd-run --user` starts a process outside every sandbox), the SSH agent (signs with the user's keys), gpg-agent, the Docker socket (root on the machine). That is a way out, not a network setting. |
| **Refuse every socket family but netlink, and keep `socketpair`** | No TCP, no UDP, no loopback, no Unix socket of anyone else's. A command's own processes still talk through pipes and socket pairs. Netlink reaches only the kernel: the C library lists interfaces through it. |
| Network namespaces with a loopback of their own | Needs unprivileged user namespaces, which Ubuntu restricts by default ([0009](0009-sandboxing.md)). |
| Landlock's TCP port rules | Ports, not hosts or addresses, and from version 4 only. |

**Failing closed.** There was no real alternative: [decision 0028](0028-enforced-values-fail-closed.md) says a restriction that is dropped is worse than a request that is refused.

## Decision

**Landlock 3 or newer enforces.** Version 1 or 2 is reported as not enforcing, with the version and what it lacks named (`NoSandbox::LandlockTooOld`), and commands then run unconfined, exactly as on a kernel without Landlock: the status line says so, and `autonomous` is refused. On versions 3 to 5 the sandbox enforces, and the session is told, in one line at its start, that a command can still signal a process outside the sandbox. From version 6 there is no gap.

**The launcher applies the version the harness counted on, and nothing less.** The policy carries the version detection found. The launcher asks the kernel for every right of that version as a hard requirement; if one is missing, nothing is applied in part and the command does not run. The rights handled are all of that version's except two, `ioctl` on devices (so that a program driving a GPU or a terminal it opened is not stopped) and the resolving of a Unix socket's path (seccomp settles that on every kernel).

**With the network off, a command can reach:**

| | Reached? |
|---|---|
| TCP and UDP, to another machine or to this one (`127.0.0.1`, `::1`) | No. The socket cannot be made. |
| Listening on a port, loopback included | No. |
| A Unix socket by its path (D-Bus, the SSH agent, Docker), or an abstract one | No. The socket cannot be made. |
| A raw or packet socket | No. |
| A connected pair of Unix sockets (`socketpair`), pipes | Yes: they reach only the command's own processes. |
| Netlink (the kernel's interface list) | Yes: it reaches no other machine and no other process. |

With `sandbox.network = "full"` nothing about sockets is filtered.

**What else the filter refuses,** with `EPERM`: `io_uring`, which can make a socket without the `socket` call; a new user namespace (`unshare` and `clone` with `CLONE_NEWUSER`, and `clone3`, answered `ENOSYS` so the C library falls back to `clone`); the kernel's keyrings; `bpf`; `perf_event_open`; `userfaultfd`; and pushing characters into a terminal (`TIOCSTI`, `TIOCLINUX`). A 32-bit system call kills the process: the filter is for the build's own architecture. On x86-64 the x32 form of each refused call is refused too.

**Every failure to apply the sandbox fails the call,** and says why: a policy the launcher does not understand, a Landlock right the kernel refuses, a filter that does not load, a build without the backends. The launcher writes one line that begins `cortex-sandbox:` and exits with 125; the command does not run.

## Why

- **Version 3 is where "writes outside the workspace fail" becomes true.** That is the done criterion of decision 0009's sandbox, and the line between confining and not.
- **Signals are disruption, not escape.** A command that signals a process outside the sandbox reads nothing it could not read, writes nothing outside the workspace, and sends nothing anywhere. Refusing `autonomous` on every kernel before 6.12 for it would cost the most common LTS kernels the level, for a gap the status line can state.
- **A Unix socket is a door, not a network.** The session bus starts processes on request; the agent signs; the Docker socket is root. Off must mean that none of them answers.
- **One rule on every kernel.** Unix sockets are refused by seccomp whatever Landlock version is there, so what a command can reach does not depend on the kernel.

## What is given up

- **Test suites that start a server on loopback,** or a Unix-socket server, fail inside the sandbox with the network off. Their way out is `sandbox.network = "full"` for the project, or running the command outside the sandbox with approval.
- **Tools that talk to a local daemon** fail in the sandbox: `docker`, `git commit -S` through gpg-agent, `ssh` through its agent, `systemd-run`. The same way out.
- **On Landlock 3 to 5** a command can signal the user's other processes. The status says so.
- **A 32-bit program** is killed in the sandbox.
- **Reads are not restricted** ([0009](0009-sandboxing.md)). The launcher can keep reads to a list of roots, and no setting asks for that yet ([outstanding/a-setting-that-restricts-reads.md](../outstanding/a-setting-that-restricts-reads.md)).

## Revisit when

- Landlock 9 (Linux 7.1) is common: its Unix-socket rule could let a command reach a socket in the workspace while still refusing the user's agents.
- Unprivileged network namespaces become reliable: a loopback of the command's own would let local test servers run with the network off.
- Users find loopback-only test suites common enough that a third network setting is wanted.
