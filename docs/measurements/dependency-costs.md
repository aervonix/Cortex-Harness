# Dependency costs

What each candidate dependency costs in build time, binary size, crate count and start-up, and what the build profile choices cost. Taken on 2026-11-12. These numbers sit behind most of the "which crate" decisions and behind the size and build-time budgets.

The spike code is throwaway and is not in this repo.

## Method

- **One tiny program per candidate.** A workspace of small binary crates, each depending on one candidate and using it enough that the linker cannot discard it: the HTTP variants build a client and a streaming request, the terminal variant draws a frame, the engines evaluate a script, and so on. An empty program (`base`) is the floor.
- **Clean builds, timed.** For each program the target directory is deleted, then `cargo build` is timed, once in the dev profile and once in release. Sources were fetched beforehand, so no download time is included.
- **Pinned to 4 cores** (`taskset`, `-j4`) on the desktop, to approximate a GitHub-hosted runner's core count. The desktop's cores are faster than a runner's, so runner times will be longer: the working assumption is about double on Linux and more on Windows, to be replaced by real timings in the repo skeleton step.
- **Size** is the release binary, stripped. "Added" is that size minus the empty program's 310 KB.
- **Crates** is the number of distinct packages in the build, counting build dependencies.
- **Release profile for the per-candidate table:** `opt-level = 3`, thin LTO, one codegen unit, `panic = "abort"`, stripped. The profile comparison further down varies these.
- **Machine:** Intel Core i7-14700K, Linux 6.18 under WSL 2, Rust 1.99.1, the default linker.
- **One run each.** Build times repeat to within a few percent. Treat differences under about 10% as noise.

## Per candidate

| Candidate | Dev build | Release build | Size added | Crates | Chosen? |
|---|---|---|---|---|---|
| Empty program | 0.5 s | 1.5 s | 0 (310 KB) | 1 | |
| **Async runtime** | | | | | |
| tokio, current-thread | 4.0 s | 6.8 s | 187 KB | 14 | Yes ([0003](../decisions/0003-async-runtime.md)) |
| tokio, multi-thread | 3.1 s | 7.3 s | 267 KB | 14 | No |
| **HTTP and TLS** | | | | | |
| reqwest, rustls, aws-lc-rs | 13.7 s | 25.9 s | 5,053 KB | 105 | No |
| reqwest, rustls, ring | 8.6 s | 15.9 s | 3,104 KB | 101 | Yes ([0004](../decisions/0004-http-and-tls.md)) |
| hyper, hyper-util, hyper-rustls, ring | 6.8 s | 13.4 s | 2,450 KB | 56 | No |
| ureq (blocking) | 5.0 s | 12.0 s | 1,892 KB | 31 | No |
| **Command line** | | | | | |
| clap with derive | 1.6 s | 4.7 s | 308 KB | 18 | No |
| lexopt | 0.3 s | 1.6 s | 18 KB | 2 | Yes ([0011](../decisions/0011-cli-parsing.md)) |
| **Terminal UI** | | | | | |
| ratatui with crossterm | 4.3 s | 6.7 s | 240 KB | 64 | No; crossterm alone is used ([0002](../decisions/0002-tui-rendering.md)) |
| pulldown-cmark | 0.9 s | 3.0 s | 379 KB | 5 | Yes |
| syntect, pure-Rust regex, default grammars | 5.2 s | 11.4 s | 2,254 KB | 45 | Yes, behind a feature ([0015](../decisions/0015-syntax-highlighting.md)) |
| **Data and text** | | | | | |
| serde, serde_json, toml | 2.5 s | 4.2 s | 259 KB | 18 | Yes |
| schemars (with the above) | 3.1 s | 5.5 s | 148 KB | 24 | Yes, for schema generation only |
| similar (diffing) | 1.1 s | 12.5 s | 450 KB | 2 | Yes |
| ignore, grep-searcher, grep-regex, globset | 2.9 s | 8.3 s | 2,110 KB | 27 | Yes ([0014](../decisions/0014-search-in-process.md)) |
| rusqlite with bundled SQLite | 5.0 s | 28.6 s | 1,954 KB | 15 | No ([0006](../decisions/0006-session-storage.md)) |
| **Logging** | | | | | |
| tracing, tracing-subscriber (formatter and registry), tracing-appender | 2.7 s | 5.0 s | 202 KB | 27 | tracing yes; our own subscriber ([0019](../decisions/0019-logging.md)) |
| **Credentials** | | | | | |
| keyring, default features, Linux | 11.0 s | 26.1 s | 2,122 KB | 102 | Behind a feature on Linux ([0018](../decisions/0018-credential-storage.md)) |
| **MCP** | | | | | |
| rmcp, client, child-process transport | 9.0 s | 27.6 s | 1,758 KB | 60 | Yes |
| rmcp, plus streamable HTTP and auth | 13.1 s | 45.3 s | 3,590 KB | 128 | Yes; shares the HTTP stack with the provider client |
| **Script and WASM engines** | | | | | |
| QuickJS (rquickjs) | 9.4 s | 22.2 s | 1,165 KB | 11 | Yes, provisional ([0005](../decisions/0005-extension-mechanism.md)) |
| Lua 5.4 (mlua, vendored) | 3.4 s | 8.3 s | 427 KB | 23 | Fallback |
| Luau (mlua) | 12.3 s | 24.6 s | 1,681 KB | 22 | No |
| Rhai | 8.1 s | 24.2 s | 2,348 KB | 27 | No |
| Boa | 16.9 s | 74.1 s | 7,013 KB | 133 | No |
| wasmtime: runtime, Cranelift, component model | 24.5 s | 86.1 s | 10,541 KB | 98 | No |
| wasmtime, default features | 33.3 s | 102.3 s | 14,443 KB | 147 | No |

Candidates measured alone overlap: most share `syn`, `serde`, `regex` and the like. The combined sets below are the numbers that matter for the budgets.

## Combined sets

"Core set" is what the harness cannot do without: tokio, reqwest with rustls and ring, serde with JSON and TOML, schemars, futures-util, bytes, ratatui with crossterm (standing in for the terminal layer; the real design uses crossterm alone), lexopt, tracing with a subscriber, the search libraries, similar, pulldown-cmark, thiserror, unicode-width. The program builds an HTTP client, starts a runtime and touches each library.

| Set | Dev build | Release build | Release binary | Crates |
|---|---|---|---|---|
| Core set | 15.1 to 15.3 s | 37.0 to 38.2 s | 6,988 KB | 191 |
| Core set, current-thread runtime | 13.8 s | 35.1 s | 6,988 KB | 191 |
| Core set with aws-lc-rs in place of ring | 19.6 s | 39.9 s | 8,941 KB | 196 |
| Core set + MCP with HTTP + keyring + syntect | 26.5 s | 76.1 s | 11,970 KB | 304 |
| Core set + QuickJS | 18.6 s | 46.8 s | 8,140 KB | 194 |
| Core set + Lua 5.4 | 15.9 s | 39.7 s | 7,379 KB | 200 |
| Core set + Rhai | 20.8 s | 49.2 s | 9,425 KB | 206 |
| Core set + Boa | 25.8 s | 93.4 s | 13,856 KB | 261 |
| Core set + wasmtime (trimmed) | 34.9 s | 103.1 s | 17,459 KB | 248 |

The core set was built in both measurement passes; the range is the two results.

**What an engine adds to the harness** (the engine rows minus the core set from the same pass):

| Engine | Dev build | Release build | Binary | Crates |
|---|---|---|---|---|
| QuickJS | + 3.3 s | + 8.6 s | + 1.1 MB | + 3 |
| Lua 5.4 | + 0.6 s | + 1.5 s | + 0.4 MB | + 9 |
| Rhai | + 5.5 s | + 11.0 s | + 2.4 MB | + 15 |
| Boa | + 10.5 s | + 55.2 s | + 6.7 MB | + 70 |
| wasmtime (trimmed) | + 19.6 s | + 64.9 s | + 10.2 MB | + 57 |

**Estimate for the first release's third-party set** (core set, MCP with HTTP, syntect, QuickJS; keyring off on Linux). This combination was not built as one program. Taking the measured set with MCP, keyring and syntect, removing keyring and adding QuickJS gives about 30 s clean in dev, about 80 s in release on 4 desktop cores, and 11 to 12 MB of binary before any of our own code. The size budget of 16 MB in [../performance-budgets.md](../performance-budgets.md) is set from that.

## Build profiles

The core set, clean release builds on 4 cores, varying the profile. Sizes are stripped.

| LTO | Codegen units | Opt level | Panic | Build | Binary |
|---|---|---|---|---|---|
| off | 16 | 3 | abort | 31.6 s | 7.77 MB |
| thin | 16 | 3 | abort | 34.8 s | 8.15 MB |
| thin | 1 | 3 | abort | 36.6 s | 7.16 MB |
| fat | 1 | 3 | abort | 51.9 s | 6.66 MB |
| fat | 1 | s | abort | 38.7 s | 5.25 MB |
| fat | 1 | z | abort | 34.2 s | 5.06 MB |
| thin | 1 | 3 | abort (second pass) | 38.5 s | 7.16 MB |
| thin | 1 | 3 | unwind | 38.4 s | 7.42 MB |
| thin | 16 | 3 | unwind | 37.2 s | 8.39 MB |
| fat | 1 | 3 | abort (second pass) | 54.1 s | 6.66 MB |
| fat | 1 | 3 | unwind | 54.6 s | 6.83 MB |

Unwinding instead of aborting on panic costs 0.17 to 0.27 MB (2.5 to 3.7%) and no build time. Two abort configurations were built in both passes: the sizes agree to within a kilobyte and the times differ by 2 s, which is the run-to-run spread to keep in mind when reading this table.

The choices made from this are in [decision 0012](../decisions/0012-release-profile.md).

## Incremental builds

The core set, dev profile.

| Action | 4 cores | 28 cores |
|---|---|---|
| Clean build | 14.3 s | 8.6 s |
| Rebuild after touching the top crate's source (three runs) | 0.38 to 0.41 s | 0.38 to 0.39 s |
| No-op build | 0.13 s | 0.13 s |
| Clean release build | 36.7 s | 22.7 s |

The relink floor with the whole dependency set is 0.4 s with the default linker, so the 5-second budget for "edit, rebuild, run" is spent on our own crates, not on linking. A faster linker was not needed to get there on Linux.

## Start-up costs

Measured inside the core-set program, five runs, and with hyperfine (200 runs after 10 warm-ups) for whole-process times.

| What | Cost |
|---|---|
| An empty Rust program, start to exit | 0.32 ms, 3.0 MB peak resident |
| The core-set program, `--version` (parse arguments, print, exit) | 0.50 ms, 3.8 MB |
| The core-set program creating a runtime and an HTTP client, then exiting | 1.8 to 2.0 ms, 6.1 to 6.3 MB |
| Creating a current-thread tokio runtime | 23 to 35 µs |
| Creating a multi-thread tokio runtime with 2 workers | 89 to 105 µs |
| Installing the TLS provider | 1 to 4 µs |
| Building the HTTP client | 1.1 to 1.5 ms warm; 45 ms on the first run after the build, when the system certificate store was not in the file cache |
| Resident memory holding a runtime and an HTTP client | 6.4 MB (1 thread with the current-thread runtime, 3 with the multi-thread one) |
| A minimal program parsing flags with clap, and with lexopt | 0.36 ms and 0.34 ms (an empty program in the same pass: 0.34 ms) |
| The core-set program with QuickJS linked but not used, `--version` | 0.55 ms and 4.0 MB, against 0.54 ms and 3.9 MB without it |
| Loading syntect's packed grammar and theme sets | 0.6 to 0.7 ms |
| Highlighting 200 short Rust lines from cold, including first-use compilation of the grammar's patterns | 27 ms |

For comparison, an empty Node program takes 13 ms and 42 MB, and an empty Bun program 1.9 ms and 12 MB ([tui-language-spike.md](tui-language-spike.md)).

**What follows.** The HTTP client is built lazily, off the start-up path. The runtime is cheap enough to create at start. Nothing in the core set threatens the cold-start budget; the things that could (certificates, grammars, the script engine, the catalog, statistics) are all loaded on first use.

## Licences

The planned set was checked against the project's MIT licence: 480 crates across the Linux and Windows dependency graphs. All are under permissive licences (MIT, Apache-2.0, BSD, ISC, Zlib, Unicode, CC0, Unlicense, BSL-1.0), with these notes:

| Crate | Licence | Note |
|---|---|---|
| `webpki-roots`, `webpki-root-certs` | CDLA-Permissive-2.0 | Permissive, for data. On the allow-list. |
| `r-efi` | MIT OR Apache-2.0 OR LGPL-2.1+ | The MIT option is taken. |
| `smartstring` | MPL-2.0+ | Reached only through Rhai, which is not used. |
| `terminfo` | WTFPL | In the lockfile only, through optional back ends of the terminal framework that are not enabled, so it is never built. The framework itself is not in the design. |
| `termina` | MIT OR MPL-2.0 | The same: lockfile only. The MIT option would apply. |

The allow-list that `cargo deny` enforces is in [../testing-and-ci.md](../testing-and-ci.md).

## What was not measured

- **Windows.** No build times or sizes on Windows in this spike. The repo skeleton step has since built the planned set on a hosted Windows runner, ring and QuickJS included: the numbers are in [../testing-and-ci.md](../testing-and-ci.md). Binary size on Windows is still not measured.
- **A hosted runner.** Not in this spike. The repo skeleton step measured both hosted runners, and its numbers replaced the estimate in [../testing-and-ci.md](../testing-and-ci.md). Against this page's estimate of about 30 s for the first release's set, a hosted Linux runner took 42 to 45 s and a Windows runner 77 s with unoptimised dependencies, so the working assumption above ("about double on Linux and more on Windows") was fair for that case. What the estimate did not include is the dev profile's `opt-level = 1` for dependencies: with it the same builds took 129 s and 259 s. The method above does not record an optimisation level for dependencies, and its figures match the unoptimised case.
- **Our own code.** The harness's crates add build time and size on top. The budgets leave room for it; the performance pass measures it.
- **Alternative linkers and code generators.** The default was fast enough on Linux. The skeleton step tries the faster linker on Windows if the measured time needs it.
