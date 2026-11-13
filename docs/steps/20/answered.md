# Answered in step 20, on 2026-11-13

Step 20 built `cortex-term`, the terminal layer. Before it started, the owner had the Windows Terminal checklist run.

| Question | The owner's answer, 2026-11-13 | Recorded in |
|---|---|---|
| **The Windows Terminal checklist** ([measurements/windows-terminal-checklist.md](../../measurements/windows-terminal-checklist.md)), on which decisions 0001 and 0002 were provisional. | **Run in Windows Terminal 1.24.** The Rust line renderer was fine on every check; the old console host printed escape sequences literally, because nothing turned on virtual-terminal processing; check 12, over SSH, was not run. Both decisions are confirmed as they stand, and the terminal layer turns on virtual-terminal processing and restores the console's modes on every exit path. | Decisions [0001](../../decisions/0001-tui-language.md) and [0002](../../decisions/0002-tui-rendering.md), "Checked on Windows" |
