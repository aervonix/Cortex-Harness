# 0018: Credentials in an owner-only file on Linux, Credential Manager on Windows

- **Status:** Accepted by the owner on 2026-11-12.
- **Date:** 2026-11-12

## Context

API keys must be stored somewhere between runs. The audience is the broad public, on desktops, servers, containers and SSH sessions. The store is read on the way to the first request, not at start-up.

## Options considered

| Option | For | Against |
|---|---|---|
| **Environment variables only** | Nothing to store. | Unfriendly for newcomers. Keys end up in shell profiles, which is a plain file anyway. |
| **A file with owner-only permissions** | Works everywhere, including servers and containers. No dependency. Easy to explain. | The key is readable by anything running as the user. That is also true of the environment and of most developer tools' credential files. |
| **The OS keychain everywhere** | Protected by the login session; the right place on a desktop. | On Linux the Secret Service needs a desktop session bus and an unlocked keyring, which servers, containers and SSH sessions lack, so a fallback is needed anyway. A large dependency on Linux. |
| **A file encrypted with a passphrase** | Protects the key at rest. | A prompt on every start, or a cached passphrase that reintroduces the problem. |

## Measurement

The `keyring` crate with its default features, built for Linux, desktop pinned to 4 cores ([measurements/dependency-costs.md](../measurements/dependency-costs.md)):

| | Dev build | Release build | Size added | Crates |
|---|---|---|---|---|
| keyring (Linux build) | 11.0 s | 26.1 s | 2.1 MB | 102 |

On Linux almost all of that is the D-Bus client stack. On Windows the backend is a thin wrapper over system calls and costs little; that half was not measured separately.

## Decision

- **Windows:** Windows Credential Manager by default. A file as the alternative.
- **Linux:** a `credentials.toml` file in the configuration directory, created with owner-only permissions, by default. The Secret Service keychain is available with the `keychain` build feature and `credentials.store = "keychain"`.
- Lookup order: command-line flag, environment variable, the store.
- Credentials are `Secret` values in memory: not printable, not serialisable, never logged.
- All of it sits behind the `AuthMethod` layer, so sign-in flows added later store their tokens the same way. **Where the boundary is** (settled in step 9): the `AuthMethod` trait is in `cortex-llm`, and `cortex-config` may not depend on that crate. So `cortex-config` owns the stores and the lookup order, and returns the key as a `Secret` together with where it came from, never the value. `cortex-core` hands both to the provider layer's API-key `AuthMethod`. No crate gains a dependency for it.

Specified in [architecture/08-configuration.md](../architecture/08-configuration.md).

## As built, in step 9

- **The Windows wrapper.** Credential Manager is reached through `windows-native-keyring-store` with `keyring-core`: the Windows store of the `keyring` family, split out by its maintainer, who now tells applications to depend on the core crate and the one store they want. With its search feature off it is 5 packages on Windows besides the Windows bindings `cortex-base` already brings, against 16 for `keyring` itself. `cortex-config` has no `unsafe` of its own. The row in [dependencies.md](../dependencies.md) has the alternatives.
- **A stored key stays on its machine.** It is written with local-machine persistence. The wrapper's default lets Windows copy a credential to the user's other domain machines.
- **The Linux keychain is not built yet.** The `keychain` feature and the `credentials.store = "keychain"` value exist, and on Linux the value gives an error that says the backend is not in any build so far and to use the file. The Secret Service backend could not be run in the blocking CI (the runners have no keyring daemon), and a credential backend that no test has run is not shipped. It is listed in the build plan as outstanding, to be built with a non-blocking job that runs it against a real keyring daemon.
- **The file.** `credentials.toml` is written owner-only every time, so a file someone made by hand with wider permissions is private after the next key is stored. Until then it is read as it is, and reported. A lock file beside it, held with the operating system's own file lock, keeps two writers apart.

## Why

- A default has to work on every Linux machine, and only the file does.
- 102 crates and 26 s of release build for a backend that many Linux users cannot use is a poor trade for the default build.
- On Windows the keychain is always present and nearly free, so it is the default there.

## What is given up

On a Linux desktop, the default is weaker than it could be. `cortex auth status` says which store is in use, and the first-run flow mentions the keychain option where a session bus is detected.

## Revisit when

- A small, pure-Rust Secret Service client makes the Linux keychain cheap: turn the feature on by default and fall back to the file when no service answers.
- macOS becomes a target: Keychain is the default there.
