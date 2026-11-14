# ConPTY: facts a session needs before it tests a terminal on Windows

On Windows, a pseudo-terminal is a pseudo-console (ConPTY): the console host without its window, with pipes for its two ends. The test kit's driver (`cortex_testkit::pty::Session`, whose system half is `cortex_term::pty`) runs a program in one, and the terminal layer's system tests and `cargo xtask perf` run the demo there. Step 20 learned each fact below one CI round trip at a time. They are gathered here from its records ([steps/20/](steps/20/)), the terminal layer as built ([architecture/09-tui.md](architecture/09-tui.md), "The terminal layer as built (step 20)"), the open item on key latency ([outstanding/key-latency-over-conpty.md](outstanding/key-latency-over-conpty.md)) and the driver's code. The Windows facts that are not about terminals are in [AGENTS.md](../AGENTS.md).

## What the test's side sees

- **ConPTY re-renders.** The bytes the program writes do not reach the other end as written: ConPTY keeps a console buffer, and writes its own rendering of it to the output pipe. A frame may arrive split across reads, or two as one, and a synchronized update's brackets are not passed on (`cargo xtask perf` says "the terminal passed on no brackets (ConPTY re-renders)"). So a virtual terminal on the test's side checks what the screen shows, never the program's bytes. What speaks for the program on Windows is what it can be asked about itself: the terminal layer's system tests run a small shell that records the console's two modes and two code pages before and after, and says whether they match.
- **ConPTY asks the terminal for modes of its own.** It turns on focus events and Windows Terminal's win32 input mode for the console it runs, and draws with automatic wrap as it likes. On Windows those modes, read from the test's virtual terminal, say nothing about the program: the system tests check them on Linux only.
- **Input comes as win32 input records,** because ConPTY asked for them. Plain sequences that follow an Escape (`ESC x` for `Alt+x`, `ESC [1;5A`) are misread by ConPTY, and Windows Terminal never sends them to one: on Windows they are checked only in the decoder's unit tests, by sequence. A key outside the Basic Multilingual Plane arrives as two records, one for each half of its surrogate pair, and the decoder joins them.

## What the program must do

- **Turn virtual-terminal processing on.** A pseudo-console starts with `ENABLE_VIRTUAL_TERMINAL_PROCESSING` off, as the old console host has it. Without it the program's escape sequences are text, and the first frame never shows: that is what failed all three system tests when the break was made by hand ([steps/20/ci.md](steps/20/ci.md)). The system tests turn it off before they start the demo, on purpose.
- **Put the console back on every way out,** both modes and both code pages. When the input half was not put back, the shell child saw input mode `1f7` before and `3e8` after, on a normal exit, a panic and a Ctrl+Break event alike.

## What the driver must do

- **Give the child invalid standard handles.** The test runner has redirected the test process's own; a child that inherits them writes there and not to the console. The driver starts it with `STARTF_USESTDHANDLES` and `INVALID_HANDLE_VALUE` for all three, which makes it use the console, and inherits no handle.
- **A child takes its environment from the test process.** The driver refuses an environment of its own for a program in a pseudo-console (on Unix it adds one). Set what the child needs in its arguments.
- **The output ends when the console is closed, not when the program exits.** The reading thread sees `ERROR_BROKEN_PIPE` (109) once `ClosePseudoConsole` has run. Dropping the session closes the console and does not wait for the reader: waiting first would wait for ever.
- **A Ctrl+Break event goes to every process on the console,** the parent's too when it shares one. The process that sends it to a child it shares a console with ignores control events first (`cortex_term::pty::ignore_control_events`), or it is stopped with the child.

## Time

- **A key takes about 16 ms to show through ConPTY on a hosted runner,** at the median and 23 ms at the 99th percentile when the program is idle, against a budget of 2 ms. The program's own share, from reading the key to writing the frame, is 0.06 to 0.22 ms. The rest fits ConPTY passing output on at its own pace of about one frame in 16 ms. In Windows Terminal 1.24 on the owner's desktop the prototype's idle echo took 0.19 ms, so this is the runner's older ConPTY. Until the owner decides ([the open item](outstanding/key-latency-over-conpty.md)), a test holds a key's echo under ConPTY to a coarse limit (250 ms), and `cargo xtask perf` reports the program's share beside the whole.
- **Frames are paced by the system's timer.** The demo's frames, written 60 times a second while streaming, came 18.2 ms apart at the 99th percentile against a limit of 18 ms, with none dropped: the wait for the next frame is the timer's, which the terminal layer does not set finer.
- **Every wait on the screen has a deadline** (`Session::wait_for`), long enough for a slow runner: Windows runners differ in speed by a factor of two ([AGENTS.md](../AGENTS.md), "Windows facts every session needs").
