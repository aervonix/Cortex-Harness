# After step 20: the terminal layer

Step 20 built `cortex-term` (the styled-line model, the line renderer, sanitising, input decoding with Windows Terminal's win32 input mode, capability detection, the inline presenter, the frame scheduler, and raw mode with the terminal put back on every way out), the test kit's virtual terminal and pseudo-terminal driver, the demo, and `cargo xtask perf`. It adds no third-party package.

## The Windows-only rules, broken by hand

Two rules live in code that only a Windows build compiles, so `cargo xtask mutate` cannot break them. Each was broken on a scratch branch and run on a hosted Windows runner with `windows-try` (`-f crate=cortex-term -f filter=sys_`), on 2026-11-13.

| Break | What failed |
|---|---|
| Virtual-terminal processing not turned on (`our_output_mode` without `ENABLE_VIRTUAL_TERMINAL_PROCESSING`) | All three system tests then: the demo's first frame never showed under ConPTY, which had been started with the mode off as the old console host has it. |
| The console's input mode not put back (`restore_modes` without its input half) | All three system tests then: the shell child reported `restored: NO`, input mode `1f7` before and `3e8` after, on the normal exit, the panic and the Ctrl+Break event alike. |
