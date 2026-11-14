# 0032: What the Linux sandbox needs of the kernel, and what it lets a command reach

- **Status:** Accepted by the owner on 2026-11-13: the partial Landlock and the network with it off, as proposed in step 26. Amended the same day in that step, after its two independent reviews: with the network on a command makes only the internet's sockets and netlink's route one (no Unix socket), netlink is open for its route protocol only, every process stays in the command's session (a process group of its own is allowed, and dies with the tree), and a write root that holds the harness's own files or its program, or lies inside its directories, is not confined. **Amended again on 2026-11-14 after a third review** (the "After the third review" section below): the harness's own git runs inside the sandbox while commands are confined; the launcher opens each root with no symbolic link followed; the owner and extended-attribute system calls are refused while a change of mode or time is a stated limit; the tree's kill sweeps the session and, with a continuity check, never reports a clean end over a change it did not make.
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
| A connected pair of Unix sockets (`socketpair`), pipes | Yes: they reach the command's own processes. A pair of datagram sockets can also send to a datagram socket of the machine's by its path (journald's): left open ([outstanding/datagrams-from-a-socket-pair.md](../outstanding/datagrams-from-a-socket-pair.md)). |
| Netlink's route protocol (the kernel's interface list) | Yes: it reaches no other machine and no other process. Netlink's other protocols, one of which reaches other processes, are refused. |

With `sandbox.network = "full"` a command may make the internet's sockets (TCP and UDP, loopback included) and netlink's route one, **and nothing else**: a Unix socket still is not made, because the user's session bus starts processes outside every sandbox on request (`systemd-run --user`) and the Docker socket is root, so reaching them would void the confinement of files while the session still reported it enforcing (found by the first review of step 26). Nor is a virtual machine's socket to its host (`AF_VSOCK`), a raw or packet socket, or a netlink protocol but the route one (found by the second review): the filter is a list of what is allowed, not of what is refused. Network on means the network, not the machine's local services. Only `socketpair` makes Unix sockets then too.

**What else the filter refuses,** with `EPERM`: `io_uring`, which can make a socket without the `socket` call; a new user namespace (`unshare` and `clone` with `CLONE_NEWUSER`, and `clone3`, answered `ENOSYS` so the C library falls back to `clone`); the kernel's keyrings; `bpf`; `perf_event_open`; `userfaultfd`; and pushing characters into a terminal (`TIOCSTI`, `TIOCLINUX`). A 32-bit system call kills the process: the filter is for the build's own architecture. On x86-64 the x32 form of each refused call is refused too (`ioctl` by its own x32 number). **`setsid` is answered as done, and not done:** every process a command starts stays in its session. A process group of its own is allowed (`setpgid`): `timeout`, test runners and every tool that signals its child's group need one, and faking it too broke them (found by the second review of step 26). When the command ends the tree sweeps its session — stopping every member it finds and killing the stopped ones, each held by a process descriptor first so that an id the system gave to another process meanwhile is never signalled — until a long run of empty passes or a bound (`tree::unix::kill_session`), and the guard does the same for a harness that died. That clears an ordinary command's leftovers and, more often than not, a process that forks in a loop; against a fork storm it cannot be sure, and the continuity check catches what it misses (the "After the third review" section, and [the-last-writes-of-a-killed-command.md](../outstanding/the-last-writes-of-a-killed-command.md)).

**A write root that holds the harness's own files is not confined.** Landlock cannot keep a directory inside a write root from being written, so where the workspace, the temporary directory or a `sandbox.write` entry holds the harness's configuration, state or data directory (a workspace that is the home directory), or lies inside one of them, or holds the user's configuration file, as named or where a link leads (a dotfiles repository), or holds the program the harness starts as the launcher (`~/.cargo` among `sandbox.write`, with `cortex` installed in `~/.cargo/bin`; a workspace that holds the build that is running), the session reports the sandbox off (`NoSandbox::HarnessFilesWritable`) and `autonomous` is refused: a confined command could otherwise change what it is allowed, or put a program that confines nothing where the next command's launcher is started from (the program found by the second review of step 26).

**Every failure to apply the sandbox fails the call,** and says why: a policy the launcher does not understand, a Landlock right the kernel refuses, a filter that does not load, a build without the backends. The launcher writes one line that begins `cortex-sandbox:` and exits with 125; the command does not run.

## Why

- **Version 3 is where "writes outside the workspace fail" becomes true.** That is the done criterion of decision 0009's sandbox, and the line between confining and not.
- **Signals are disruption, not escape.** A command that signals a process outside the sandbox reads nothing it could not read, writes nothing outside the workspace, and sends nothing anywhere. Refusing `autonomous` on every kernel before 6.12 for it would cost the most common LTS kernels the level, for a gap the status line can state.
- **A Unix socket is a door, not a network.** The session bus starts processes on request; the agent signs; the Docker socket is root. Off must mean that none of them answers.
- **One rule on every kernel.** Unix sockets are refused by seccomp whatever Landlock version is there, so what a command can reach does not depend on the kernel.

## What is given up

- **Test suites that start a server on loopback,** or a Unix-socket server, fail inside the sandbox with the network off. Their way out is `sandbox.network = "full"` for the project, or running the command outside the sandbox with approval.
- **Tools that talk to a local daemon** fail in the sandbox, with the network on or off: `docker`, `git commit -S` through gpg-agent, `ssh` through its agent, `systemd-run`. Their way out is running the command outside the sandbox with approval.
- **A program that makes a session of its own** stays in the command's. One that then signals the group it believes it leads finds none (Node's `process.kill(-pid)` after `detached: true`), or reaches the whole command (Python's `os.killpg(os.getpgid(pid))` after `start_new_session=True`). **A program that makes a terminal fails:** without a session of its own it cannot take one as its controlling terminal, so the child of `forkpty` exits (Python's `pty.spawn`, pexpect, node-pty), and `script` runs without one. The way out is a run outside the sandbox with approval ([outstanding/a-session-of-its-own-in-the-sandbox.md](../outstanding/a-session-of-its-own-in-the-sandbox.md)).
- **Python's `multiprocessing` with the `forkserver` start method,** the default on Linux from Python 3.14, talks to its server through a Unix socket, and fails in the sandbox with the network on or off; the `fork` start method works (by the second review's reading, not run here).
- **On Landlock 3 to 5** a command can signal the user's other processes. The status says so.
- **A 32-bit program** is killed in the sandbox.
- **A command cannot change a file's owner or its extended attributes,** inside the workspace or out, because the sandbox refuses those system calls whole (seccomp cannot check their path). A tool that preserves them in a copy (`cp -a`, `rsync -aX`, `tar --xattrs`) still copies the content, the mode and the times, but drops the owner and the attributes it may not set, and exits non-zero where it was asked to preserve one it could not — `cp -a` of a file that carries an extended attribute (a POSIX ACL among them, which a default ACL on a directory gives every file under it) reports `preserving permissions … Operation not permitted` and returns non-zero, the copy itself made all the same. A build that copies ACL-bearing files under `set -e` would stop there; the way out is a run outside the sandbox with approval.
- **A command can change a file's mode and times anywhere the user owns, outside the write roots too,** though not its content there: the sandbox cannot refuse those calls without breaking builds that set modes and times. It is tampering, not a content write or exfiltration ([outstanding/modes-and-times-outside-the-write-roots.md](../outstanding/modes-and-times-outside-the-write-roots.md)).
- **A repository that uses git-lfs** loses change detection and undo for a command while an LFS file is modified: the harness's confined git cannot run the `clean` filter, which must write ([outstanding/confined-git-and-lfs-filters.md](../outstanding/confined-git-and-lfs-filters.md)). It is reported, not silent.
- **Reads are not restricted** ([0009](0009-sandboxing.md)). The launcher can keep reads to a list of roots, and no setting asks for that yet ([outstanding/a-setting-that-restricts-reads.md](../outstanding/a-setting-that-restricts-reads.md)).

## Revisit when

- Landlock 9 (Linux 7.1) is common: its Unix-socket rule could let a command reach a socket in the workspace while still refusing the user's agents.
- Unprivileged network namespaces become reliable: a loopback of the command's own would let local test servers run with the network off.
- Users find loopback-only test suites common enough that a third network setting is wanted.

## After the third review of step 26

A third independent review, on 2026-11-14, found six defects. The owner chose to fix all six in that step. What changed here:

**The harness's own git runs inside the sandbox.** A repository's configuration can name a program for git to run (a clean filter on `git status` or `git diff`, a partial clone's fetch transport, and more). A confined command may write that configuration — it lies in the workspace — after the user trusted the project. So every git the harness starts (change detection, the base state, a file's earlier content) goes through one entry point, `cortex_base::git::start`, which while commands are confined runs git under a second launcher: the session's reads, no write root, no network, no temporary directory. A program the configuration names then runs confined, as the command did, and can change nothing. Git is still asked only about a trusted project. A git that fails or is refused inside the sandbox (a filter that must write, as git-lfs's) leaves the change list incomplete, with the reason, never a list that looks complete ([outstanding/confined-git-and-lfs-filters.md](../outstanding/confined-git-and-lfs-filters.md)). On Windows, where the sandbox is off, git runs as before. The shell the harness starts, and the git program itself, are passed over when their real path lies inside a write root, where a confined command could have planted them; the search goes on down `PATH`.

**The write roots are opened with no symbolic link followed.** A confined command could replace a write root — or its own temporary directory, when it lies inside a `sandbox.write` root — with a symbolic link, and the next command's launcher would follow it and grant writes wherever it led. The harness resolves each root once, when the session's policy is made, and the launcher opens it with `openat2(RESOLVE_NO_SYMLINKS)`: a root, or any directory on the way to it, that became a link since is refused and the command does not run. A link the user set up on purpose is resolved at the start and still works.

**The owner and extended attributes are refused; the mode and the times are a stated limit.** Landlock does not govern a change of a file's owner, mode, times or extended attributes, and seccomp cannot check their path argument. After measuring what common builds need — `cargo build`, `git checkout`, `npm install`, `pip install` and `make install` — the filter refuses the `chown` family and the extended-attribute setters outright (`EPERM`), inside the workspace too: builds do not change an owner and set no extended attributes on what they write, so `cp -a` and the rest still copy what a build produces — they pass over the owner and the attributes they may not set, though a copy asked to preserve an extended attribute it then cannot (a file with a POSIX ACL) exits non-zero, the content and mode copied all the same ("What is given up"). Refusing the extended-attribute setters also stops a confined command adding an access-control list, which is an extended attribute, that would open a file to another local user. A change of **mode** or **time** stays allowed, because cargo, git checkouts and installers set them; that a confined command can change the mode and times of any file the user owns, outside the write roots too — though not its content there — is a stated limit ([outstanding/modes-and-times-outside-the-write-roots.md](../outstanding/modes-and-times-outside-the-write-roots.md)). The newest per-directory-fd attribute calls (Linux 6.13) are not yet filtered.

**The tree's kill, and never a clean end over a change from outside.** The kill of a process tree now sweeps the command's session — stopping every member it finds and killing the stopped ones — until a long run of empty passes or a bound ([the-last-writes-of-a-killed-command.md](../outstanding/the-last-writes-of-a-killed-command.md)). That clears an ordinary command's leftovers and, more often than not, a process that forks in a loop. It cannot promise to against a fork storm, which would need to freeze the whole tree at once (a control group, `cgroup.kill`, left for step 32), and under load a pass can call the session clear while a child is being born. The sweep stops at once when the user has already raised a "kill now", rather than running out the full bound (about 29 s at 1,600 processes, measured); it then reports an escape, and the continuity check covers it. Taking the sweep off the core thread, so a fresh interrupt lands while it is already running, is left to step 32 with the control-group kill. So detection does not rest on the kill winning: when the kill reaches its bound with the session alive it reports an escape and the turn stops with a warning; and, whether or not an escape is reported, the look before every confined command is compared with the look after the last one, and at the session's close against the last look — a protected path that changed in between, and that none of the harness's own tools changed, stops the turn, is recorded for undo, and is reported in neutral words (a process a command left running, or a change made by hand). A change the harness's own tools made is told apart by the paths they recorded since the last after-look, which spans rounds.

**The look's blind spots.** A main repository's `.git/commondir`, which points git's configuration and hooks elsewhere, is watched now and followed without following a link; a `.cargo` made as a link to a directory with a `config.toml` is recorded, with its configuration by name through the link; and a protected file too large to keep is compared by a hash, so a same-length change with its times put back is found on a file system whose change time the harness cannot trust. A file too large to hash marks the look incomplete.
