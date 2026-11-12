# Windows Terminal checklist

A short manual check for the owner to run on the desktop, in native Windows. It covers the one thing the TUI measurements could not: how the prototypes behave in a real Windows Terminal window.

**When.** Before step 20 of the build plan, the terminal layer. The owner accepted both decisions on 2026-11-12 on the condition that this is run first.

**Why it is manual.** The measurement harness ran on Linux (the laptop, and the desktop under WSL 2). It cannot see a Windows terminal window, and a pseudo-terminal recording does not show what a person sees. The decision in [decisions/0001-tui-language.md](../decisions/0001-tui-language.md) stays provisional until this has been run.

**Time:** about 20 minutes.

## What you need

- Windows Terminal, current stable. Note its version (Settings → About).
- A Rust toolchain for Windows, and Bun or Node for the TypeScript prototype.
- The spike folder `ch-spike-tui`. It is not in this repo. It is inside the archive of step 1's spikes, `cortex-harness-step1-spikes.tar.gz`, which the owner is keeping until this checklist has been run. It holds the stub core, the Rust prototype (`tui-rs`), the TypeScript prototype (`tui-ts`) and the benchmark harness (`tui-bench`).

## Build

In PowerShell, in the spike folder:

```powershell
cargo build --release
cd tui-ts; bun install; bun run build:node; bun run build:bun; cd ..
```

If the Rust build fails, stop and record the error. That is itself a result.

## Run each prototype by hand

Use a window of about 120 by 40. For each command below: start it, press `Enter` to start the stream, type a few letters while it streams, resize the window once while it streams, and press `Ctrl+C` to quit.

```powershell
$stub = "$PWD\target\release\stub-core.exe"
.\target\release\tui-rs.exe --mode lines  --rate 400  --turn-ms 12000
.\target\release\tui-rs.exe --mode lines  --rate 2000 --turn-ms 12000 --live-rows 30
.\target\release\tui-rs.exe --mode inline --rate 400  --turn-ms 12000
.\target\release\tui-rs.exe --mode full   --rate 400  --turn-ms 12000
node tui-ts\dist\main.mjs --stub-bin $stub --rate 400  --turn-ms 12000
node tui-ts\dist\main.mjs --stub-bin $stub --rate 2000 --turn-ms 12000 --live-rows 30
```

`lines` is the renderer the design proposes. `inline` and `full` are the framework-based comparisons. The last two are the TypeScript prototype.

## What to look for

Mark each: fine, minor, or broken, with a few words.

| # | Check | `lines` | `inline` | `full` | TypeScript |
|---|---|---|---|---|---|
| 1 | **It starts and draws.** The editor line and status line appear at once. | | | | |
| 2 | **No flicker while streaming.** The live area at the bottom does not flash or blank. | | | | |
| 3 | **Text lands in the scrollback.** After the stream, scroll up with the mouse wheel: the earlier output is there, in order, with no gaps, duplicates or stray blank lines. (Not applicable to `full`.) | | | | |
| 4 | **The window does not jump.** While streaming, the view stays at the bottom and does not leap to the top of the scrollback. | | | | |
| 5 | **Typing while streaming** shows each letter at once, and none is lost or doubled. | | | | |
| 6 | **Resize while streaming.** After making the window narrower and wider, the bottom area redraws correctly. Note any leftover lines. | | | | |
| 7 | **The 30-line case** (the two commands with `--live-rows 30`) stays smooth. | | n/a | n/a | |
| 8 | **Quitting restores the terminal.** After `Ctrl+C` the prompt is back, the cursor is visible, and typing works normally. | | | | |
| 9 | **Glyphs.** The prompt mark, the rule and the spinner show as intended in your font, not as boxes. | | | | |
| 10 | **Colours** are readable on your theme. | | | | |

Two extra checks, once each, with `tui-rs --mode lines`:

| # | Check | Result |
|---|---|---|
| 11 | Run it in the **old console host** (`conhost.exe`, for example by starting `cmd.exe` from Run) instead of Windows Terminal. Does it draw at all, and does it flicker? | |
| 12 | Run it inside Windows Terminal **over SSH into WSL or another machine**, if that is how you often work. | |

## Optional: numbers on Windows

The harness runs on Windows too, through ConPTY. It gives start-up time and key latency there (memory and CPU sampling are Linux-only in the spike).

```powershell
$b = "$PWD\target\release"
& $b\tui-bench.exe startup --name rust-lines --out results\win --runs 20 -- $b\tui-rs.exe --mode lines --rate 400
& $b\tui-bench.exe startup --name ink-node  --out results\win --runs 20 -- (Get-Command node).Source "$PWD\tui-ts\dist\main.mjs" --stub-bin $b\stub-core.exe --rate 400
& $b\tui-bench.exe run --name rust-lines --out results\win -- $b\tui-rs.exe --mode lines --rate 2000 --turn-ms 12000 --stats '{STATS}'
& $b\tui-bench.exe run --name ink-node  --out results\win -- (Get-Command node).Source "$PWD\tui-ts\dist\main.mjs" --stub-bin $b\stub-core.exe --rate 2000 --turn-ms 12000
```

Each prints one JSON line. The fields to copy back are `first_frame_ms`, `input_latency_ms.streaming` and `frames.dropped_pct`. Under ConPTY the flicker fields describe what the console layer passed on, not what the window painted, so trust your eyes for flicker.

## Also worth five minutes: the script engine on Windows

The extension design depends on a C-based JavaScript engine building with the Windows toolchain ([decisions/0005-extension-mechanism.md](../decisions/0005-extension-mechanism.md)). In the `ch-spike-ext` folder:

```powershell
cargo build --release -p ext-bench
```

Record whether it builds. If it does not, record which crate failed. The build plan's extension step repeats this check on CI and switches to the fallback language if needed, so a failure here is useful early warning, not a blocker.

## What to send back

- The Windows Terminal version and the Windows build.
- The filled-in tables.
- Any numbers from the optional section.
- Whether the engine built.

## How the result changes the plan

| Result | Consequence |
|---|---|
| `lines` is fine on all checks | Decision 0001 and 0002 can be confirmed as they stand. |
| `lines` has trouble with scrollback or jumping (checks 3, 4, 6) and `full` is fine | Inline stays the design, and full screen becomes the default on Windows until the inline presenter is fixed there. |
| Both Rust modes are broken where the TypeScript prototype is fine | Decision 0001 is reopened. This is not expected: all of them send the same kinds of terminal sequences. |
| The old console host is broken (check 11) | Documented as unsupported for the TUI. Headless mode still works there. |
