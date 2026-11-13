# 0029: Owner-only files on Windows are made in `cortex-base`, in its one module that may use `unsafe`

- **Status:** Accepted by the owner on 2026-11-12.
- **Date:** 2026-11-12

## Context

Session logs, credential files and state files must be readable by their user and nobody else ([security.md](../security.md)). On Linux that is a mode, `0600`, which the standard library can give a file as it creates it, and step 4 built it into `write_atomic` as the `owner_only` option.

On Windows a new file takes the access rules of its folder. To give a file rules of its own, the rules have to be handed to the call that creates it, and the standard library has no way to do that. It takes the Windows API, which is `unsafe` to call. The lint rule so far allowed `unsafe` in `cortex-sandbox` and `cortex-term` only, so step 4 left `owner_only` doing nothing on Windows and listed the gap as outstanding.

Steps 8 (sessions) and 9 (configuration and credentials) both promise owner-only files on both OSes, and run at the same time.

## Options considered

| Option | For | Against |
|---|---|---|
| **Rely on the folder.** Files go under the user's profile (`%APPDATA%`, `%LOCALAPPDATA%`), whose default rules already keep other ordinary users out. | No code and no `unsafe`. | It is a default, not a guarantee. The data directory and the configuration path are configurable, a profile on a shared or redirected volume can have other rules, and the folder's list always includes the Administrators group and SYSTEM. "Owner-only on both OSes" would mean two different things, and no test could say which machine has which. |
| **A helper in `cortex-sandbox`,** which may already use `unsafe`, called by `cortex-session` and `cortex-config`. | No change to the rule about where `unsafe` lives. | Two crates that have nothing to do with sandboxing would depend on the sandbox crate, against the dependency direction in [01-workspace.md](../architecture/01-workspace.md). And the atomic write, which is in `cortex-base`, could not use it, so every caller would have to protect the file itself after writing it, which leaves a moment when it is not protected. |
| **Do it in `cortex-base`,** in one Windows-only module that is allowed `unsafe`. | One audited place. Every crate that writes a private file already depends on `cortex-base` and gets the same behaviour from the same option, `owner_only`, that already works on Linux. The access list goes to the call that creates the file, inside the atomic write. | The rule changes: `cortex-base`, the crate everything leans on, gains `unsafe` and a Windows-only dependency. |

## Decision

**`cortex-base` makes owner-only files and directories on both OSes. On Windows it does so in one module, `private::windows`, which is the only place in the crate where `unsafe` is allowed.**

- **The rule.** `unsafe_code` stays denied for the workspace. It is allowed per module in `cortex-sandbox` and `cortex-term`, and in exactly one module of `cortex-base`: `crates/cortex-base/src/private/windows.rs`. Every block has a `// SAFETY:` comment, which Clippy requires. Nothing else in `cortex-base` may use `unsafe`, and no other crate gains the right by this record.
- **The dependency.** `windows-sys`, the raw bindings Microsoft publishes, with the five API families the module calls and no more. It is compiled on Windows only.
- **What "owner-only" is on Windows.** An access list with one entry, which allows the current user full control, with inheritance from the folder turned off, and the current user as the owner. That is what mode `0600` means, and `0700` for a directory.
- **Nobody else is on the list.** Not SYSTEM and not the Administrators group. Both reach any file regardless: an administrator can take ownership, and backup software reads with a privilege that passes access lists by. Leaving them off breaks nothing that works and keeps the list equal to the Linux mode. If something is found that needs one of them, it is added by amending this record, not quietly.
- **The list is there before the file is.** It is given to the call that creates the file or the directory. A file is never created first and protected afterwards: a program that opened it in between would keep its handle after the rules changed.
- **A private file never relies on its folder.** A directory the module creates hands its list down to what is created inside it later, as a second line of defence. But a directory that already exists is left exactly as it is, because its permissions may be the user's choice. So every private file is made private itself, through `private::create_new` or `write_atomic` with `owner_only`.
- **`owner_only` means the same on both OSes, for a new file and for an existing one.** With it, the file the write leaves is owner-only. Step 4's Linux behaviour kept the mode of an existing file even with the option set, so a credentials file someone had created by hand as `0644` stayed readable by others after a key was written into it. That is changed: the option only ever tightens. Without the option nothing changes: a replaced file keeps its mode on Linux and takes its folder's rules on Windows.

**The API** (`cortex_base::private`), all of it blocking:

| Function | What it does |
|---|---|
| `create_new(path)` | Creates a new, empty file that only the current user can open, and returns it open for writing. Fails if the name exists, a link included: nothing is followed. |
| `create_dir_all(path)` | Creates a directory, and any missing parents, each one owner-only. Existing directories are left alone. |
| `is_private(path)` | Linux: no permission bit for the group or others. Windows: owned by the current user, inheritance off, and every entry allows the current user. A deny entry, an entry for anyone else, an inherited entry or no list at all makes the answer no. |
| `access_list(path)` (Windows) | The owner and the entries read back from the file system, for tests and for diagnostics. |

## Why

- **One meaning.** "Owner-only" is a promise about a file, and it should be the same promise on both OSes, checked by a test that reads the access list back.
- **The right layer.** The atomic write is where a private file is born. Protecting it anywhere else means protecting it later.
- **The smallest audit.** One file of about 500 lines, a third of it comments, holds every `unsafe` block in the crate. The descriptor is built from a string in Windows's own description language, so no access list is laid out by hand; what Windows allocates is freed by a `Drop`; and handles are owned by standard-library types from the moment they exist.

## What is given up

- **`cortex-base` is no longer free of `unsafe`.** The module was reviewed independently before it was merged, and its tests run on the Windows CI job, where they read the access list back with the module's own reader and, separately, with `icacls`, a program that shares no code with it.
- **A file's owner can open it and nobody else can, which includes tools the user runs as another account.** A service that indexes or backs up files as its own user will be refused, as it would be by `0600` on Linux.
- **A file system with no access lists cannot keep the promise.** On FAT or exFAT, and on some network and virtual shares, Windows creates the file and drops the list without saying so. The module does not refuse to write there: a session on a USB stick still has to be saved. `is_private` then answers no, or fails, and the caller reports it. The same is true of a mode on Linux on such a file system.
- **A process that runs under a restricted token may be refused its own user's private files.** Windows checks a restricted token's restricting identifiers as well as the user, and the list names the user alone. Nothing runs that way today. The Windows sandbox step (49), which starts commands under such a token, has to decide what those commands may read, and private files of the harness are not among it.

## What the review found

The module was reviewed independently before its first push, block by block against Microsoft's documentation and the `windows-sys` source: who allocates and who frees, what a failure returns, whether an error is read before anything can overwrite it, the sizes and alignment of every buffer, and whether a file can exist without its list. It found no memory-safety defect, no allocation leaked or freed twice, and no error read out of order. It could not settle by reading two things that only Windows can answer, and the tests on the Windows CI job answer both: that the descriptor string is accepted as written, and that the "protected" flag survives the creation of the file. It also pointed out the two limits above, and that the user is read from the process, not from the thread: a thread that impersonated someone else would create a file for the process's user. The harness never impersonates.

## Revisit when

- The standard library gains a way to create a file with a security descriptor. The module is then replaced by that, and the allowance goes.
- A real case turns up that needs SYSTEM or the Administrators group on the list.
