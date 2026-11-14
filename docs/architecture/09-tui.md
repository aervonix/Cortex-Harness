# The terminal UI

The interactive front end. It is written in Rust and lives in the main binary, in two crates: `cortex-term` (terminal layer and renderer, no knowledge of agents) and `cortex-tui` (the application). The language choice is [decision 0001](../decisions/0001-tui-language.md) and the rendering approach is [decision 0002](../decisions/0002-tui-rendering.md). The owner accepted both on 2026-11-12, and both were confirmed on 2026-11-13 by the Windows Terminal checklist.

The look described here is our own: its glyphs, colours and wording are ours, in the arrangement modern terminal agents share. Glyphs, wording, colours, commands and default keys are defaults. The owner accepted a first draft on 2026-11-12, reopened it, and chose this look on 2026-11-14 after three rounds of proposals ([decision 0033](../decisions/0033-tui-look.md)). The TUI build steps implement what this page says and do not need to invent more.

It follows the [vision](../vision.md)'s interface policy: a clean chat-style flow by default, panels that open when there is something to show and close again, and the logo's purple (`#7A3CFB`) as the accent colour.

## Rendering approach

### Inline by default

The transcript lives in the terminal's own scrollback. The harness owns only a small **live region** at the bottom of the screen.

```
 ┌───────────────────────────── terminal window ─────────────────────────────┐
 │  … earlier output, owned by the terminal: scroll, search, select, copy …   │
 │                                                                            │
 │  committed transcript lines (written once, never touched again)            │
 │                                                                            │
 ├──────────────────── live region (owned by the harness) ────────────────────┤
 │  the unfinished tail of what is streaming                                  │
 │  the running tool round, in its box                                        │
 │  task list, approval dialog or completion menu, when present               │
 │  ╭ ◆ the input box ───────────────────────────────────────────────────╮    │
 │  hint row: the keys and the status that matter now                         │
 └────────────────────────────────────────────────────────────────────────────┘
```

**Why.** The terminal's scrollback, search, selection and copy keep working, including over SSH and inside multiplexers. Nothing the harness has printed needs to be kept in memory or redrawn, so the cost of a frame depends on the size of the live region and not on the length of the session. The measurements behind this are in decision 0002.

**The rule that makes it flicker-free: commit as soon as stable.** Content moves from the live region to the scrollback the moment it can no longer change. The live region therefore stays a few lines tall however long the answer is.

| Content | Committed when |
|---|---|
| Paragraph text | A wrapped line is committed when the next line begins. Only the last, growing line stays live. |
| Code block | Line by line, as each line ends. |
| List, quote, heading | Line by line. |
| Table | When the table ends, because column widths depend on every row. A table taller than the live region's limit is committed in parts with the widths known so far. |
| Inline markup still open at the end of a line (bold, code) | The line is held for at most two more lines, then committed as it is. |
| Reasoning | Not committed while streaming. Replaced by its summary (`∴ Reasoned for 6s` and the provider's summary) when it ends. |
| Tool round | Live while any of its calls runs. The round's box, with each call's line and any open detail, is committed when its last call completes ("Tool rounds", below). |
| User prompt | On submit. |

**The price.** Committed lines are wrapped for the width at the time. If the window is resized, earlier output is not re-wrapped by the harness. Collapsing or re-expanding old output in place is not possible either; long content is opened in the viewer (below) instead.

### Full screen as the alternative

A second presenter owns the whole screen (the terminal's alternate screen). It keeps the transcript itself, scrolls it, re-wraps on resize and restores the user's previous screen on exit.

- Selected with `--fullscreen` or `tui.presenter = "fullscreen"`.
- It exists for terminals and multiplexers where inline drawing misbehaves, and for people who prefer an application-style view.
- It is built after the inline presenter, on the same renderer, and shows the same components.

### One renderer, two presenters

Every component renders to **styled lines** for a given width. A presenter decides where lines go.

```
items ─► components ─► styled lines ─┬─► inline presenter ─► commits + live region
                                     └─► full-screen presenter ─► viewport over all lines
```

The renderer is ours and line-based. The reasons for not using a cell-grid TUI framework's terminal are in decision 0002: the measured prototype built on one sent more bytes per frame than a line-based renderer needs to, its inline mode has a fixed height and relies on asking the terminal for the cursor position, and other Rust harnesses that started with it replaced that part.

### How a frame is drawn (inline)

1. Collect lines committed since the last frame, and the new live-region lines.
2. Build the frame in one buffer:
   1. begin a synchronized update;
   2. move the cursor to the first row of the live region;
   3. write each newly committed line, each followed by "erase to end of line" and a line break; the terminal scrolls by itself as needed;
   4. write the live region: if nothing was committed and its height is unchanged, only the lines that differ from the previous frame; otherwise all of it, then "erase below";
   5. place the cursor at the editor's insertion point;
   6. end the synchronized update.
3. Write the buffer with a single system call.

Properties this gives, each checked by tests ([../testing-and-ci.md](../testing-and-ci.md)):

- Nothing is ever cleared and then redrawn. Lines are overwritten in place.
- A frame is one write, wrapped in a synchronized update, so a terminal that supports the mode shows it atomically, and one that does not receives so little at once that tearing is rare.
- No scroll-region or cursor-position queries are needed, which avoids the terminals and multiplexers that mishandle them.
- Nothing above the live region is touched.

**Live region height.** At most half the window, and at least the input box and the hint row. Content that would exceed it is clipped with a marker (for example a tool's output tail shows fewer lines). The presenter itself only refuses a region taller than the screen: it keeps the region's bottom rows, because rows that had scrolled away could not be reached again.

**The last column stays empty.** A line may use every column but the last (`Inline::width`). A row that filled the last column would leave the terminal waiting to wrap, which terminals handle differently (some wrap at once, and the next line break then leaves a blank row), and the erase after it would take its last character. Lines are cut to that width by the renderer, between grapheme clusters, so a line is always one row.

**Resize.** On a width change the live region is redrawn at the new width, row over row, and whatever is left below it is erased. Because the terminal may have re-wrapped the old live region itself, a few stale lines can be left in the scrollback after a shrink. `Ctrl+L` redraws the live region. The full-screen presenter has no such limit.

### Frame scheduling

- The UI loop sleeps until something happens: a key, a core event, a resize, or an animation tick while something is running.
- **Stream-driven frames** are capped at 60 per second. Events that arrive between frames are merged.
- **Input-driven frames** are drawn immediately. A key press never waits for the frame timer.
- A frame is skipped when the lines to draw equal the lines on screen.
- When idle, nothing is scheduled.

The budgets for frame time, input latency and idle CPU are in [../performance-budgets.md](../performance-budgets.md).

### Rendering cost control

- Each transcript item caches its rendered lines by content version, width and theme.
- Streaming markdown is parsed incrementally: finished blocks are never parsed again, only the block in progress.
- Syntax highlighting loads on first use, off the UI thread. Until it is ready, code is shown unhighlighted. It then applies to new lines.
- In inline mode, committed lines are dropped from memory. In full-screen mode, rendered lines are kept only around the viewport and rebuilt from items on demand.

## Layout and look

The owner chose this look on 2026-11-14, after three rounds of proposals ([decision 0033](../decisions/0033-tui-look.md)). Its reference screens are in [tui-look/](tui-look/README.md): the same session drawn at 100 and 60 columns, on a dark and a light background, in 16 colours, with no colour and in ASCII. They were generated from one source and each was drawn through the inline presenter into the test kit's virtual terminal before the choice.

- **Prose** wraps at `tui.prose_width` (default 80) or the window's width, whichever is smaller.
- **Code, diffs and boxes** use the window's width, less the last column.
- **Two indent levels:** a block's text sits two columns in, after its glyph. A tool call's detail sits two columns in from its call line, inside the round's box.

### A session, as it appears in the scrollback

```
◇ All 46 tests pass, and cargo fmt is clean.

◆ 1_000 lexes as 1. Fix scan_number so digit separators work, and add a test.

∴ Reasoned for 6s
  The loop breaks on any non-digit, so _ ends the literal. Check whether a
  trailing _ is meant to be an error first.

◇ scan_number stops at the first character that is not a digit, so 1_000 ends
  after the 1. I'll read the scanner and allow _ between digits.

╭──────────────────────────────────────────────────────────────────────────────╮
│ ✓ Read lex.rs  lines 120–178                                                 │
│ ✓ Edit lex.rs  +5 −1                                                         │
│    141       while let Some(c) = self.peek() {                               │
│    142 −         if !c.is_ascii_digit() { break; }                           │
│    142 +         if c == '_' && self.peek_next().is_some_and(|d| d.is_asci…  │
│    143 +             self.bump();                                            │
│    144 +             continue;                                               │
│    145 +         }                                                           │
│    146 +         if !c.is_ascii_digit() { break; }                           │
│    147           self.bump();                                                │
│ ✗ Run cargo test -p parser  exit 101 · 3.4s                                  │
│   test lex::numbers::trailing_separator ... FAILED                           │
│   assertion failed: lex("1_").is_err()                                       │
│   test result: FAILED. 47 passed; 1 failed; 0 ignored; finished in 0.02s     │
╰──────────────────────────────────────────────────────────────────────────────╯

◇ One test fails: 1_ must be an error, but the scanner now stops before the _
  and returns 1. A separator has to be followed by a digit, so the other case
  becomes an error:

  if c == '_' {
      if !self.peek_next().is_some_and(|d| d.is_ascii_digit()) {
          return Err(self.error("digit separator must be followed by a digit"));
```

In colour, the `◆` is the accent and your message is bold. The answer and its `◇` are at full brightness. The reasoning, the box's border and the tool detail are dim. `✓` is green and `✗` red, and the removed and added lines are tinted red and green to the box's edge.

### Elements

| Element | Appearance | Notes |
|---|---|---|
| **Your message** | `◆` in the accent, then the text in bold. Wrapped lines sit two columns in. One blank line before and after. | The input box starts with the same `◆`, so a message looks the same before and after it is sent. |
| **Answer** | `◇` in the terminal's own foreground, then the text at normal weight. Wrapped lines sit two columns in. A blank line between blocks. | The answer is the only text at full brightness outside a box. Its glyph is neither dim nor the accent: the accent means "you". Selecting and copying gives the text with a two-column indent; `/copy` gives it clean. |
| **Code block** | Under the answer, two columns in, highlighted when the feature is on. No border, no background, no language line. | A line wider than the room is **cut** with `…`, not wrapped: one source line stays one row, so the code keeps its shape and a diff keeps its line numbers. The whole line is in the viewer and in `/copy`. |
| **Inline code** | The `code` colour. No background. | |
| **Headings** | Bold. No `#`. | |
| **Lists, quotes** | `–` for an unordered item and numbers for an ordered one. A quote has a dim `│`. | |
| **Tables** | Columns aligned with spaces, and a dim rule under the header. | Falls back to stacked `key: value` rows when too wide. |
| **Links** | Underlined text: a terminal hyperlink where supported, otherwise followed by the URL in dim. | |
| **Reasoning** | While streaming, two live rows: the spinner, `Reasoning` and the elapsed time, dim; then the latest words in dim italic. When finished: `∴ Reasoned for 6s`, dim, then the provider's summary in dim italic, two columns in. | `tui.reasoning = "duration"` keeps only the first line; the summary is then in the viewer. |
| **Tool round** | A box with a dim rounded border around one round of tool calls: one line per call, and each call's detail under it when open. | See "Tool rounds", below. |
| **Diff** | Inside an open box, under its call. A gutter holds the line number (dim) and the sign, right beside the code. Added and removed lines are tinted to the box's edge, the sign takes the line's colour, and changed words are bold. Context lines are dim. | No `+` or `−` at the far left. A diff of up to `tui.tool_detail` lines starts open. |
| **Tool output** | Dim, under its call when open: a failure's last 3 lines, or a running command's last 4. | All of it is in the viewer. |
| **Notice** | `·`, then one dim sentence. Wrapped lines sit two columns in. | Retries, a speed fallback, compaction with its token counts, sandbox reports, instruction changes. A new kind of notice takes the same form. |
| **Error** | `✗` and a sentence in the error colour, then a dim sentence on what to do next, two columns in. | |
| **Session header** | At the start of a session, once: a small box with a dim rounded border holding `◆ Cortex-Harness` and the version; the model, effort, folder and branch; and one dim line on what the project profile holds. | |
| **Turn summary** | Optional dim line after a turn: duration, tokens and cost. | Off by default (`tui.turn_summary = true`). Either way, the hint row shows the last turn's cost for a while. |

### Tool rounds

**One box per round:** the calls the model asked for together. A round is what runs together, and a box for each call would add two border rows to every call.

**A call's line:** its state mark, then the tool as a bold imperative with its target (`Read lex.rs`, `Edit lex.rs`, `Run cargo test -p parser`, `Search scan_number`; an extension's tool by its own name), then two spaces and its measure, dim. An edit's measure is `+5 −1` in the diff colours, and a failure's measure (`exit 101 · 3.4s`) is in the error colour. A path is shortened to its file name; the whole path is in the viewer.

| Mark | Meaning | Measure |
|---|---|---|
| spinner (accent) | running | elapsed time |
| `?` (warning) | waiting for approval | "approval needed" |
| `✓` (success) | succeeded | lines, matches, `+4 −1`, duration |
| `✗` (error) | failed | exit code or a short reason, and the duration |
| `⊘` | denied | |
| `■` | interrupted | |

The spinner is an eight-frame braille cycle at 100 ms. `tui.reduced_motion = true` replaces it with a static mark.

**Open and closed.** Closed, a box shows only the call lines. Open, each call's detail sits under its line, two columns in. What starts open, with `tui.tool_output = "auto"` (the default):

| Detail | Starts |
|---|---|
| A failure's output | open, the last 3 lines |
| A diff of up to `tui.tool_detail` lines (default 12) | open |
| A longer diff | closed |
| A successful command's output, a read, a search | closed |
| A running command's output | closed; open, its last 4 lines |

`"always"` opens every detail, and `"never"` keeps every box closed.

**While a round runs, its box is in the live region.**
- `Ctrl+T` opens or closes it in place, and its bottom border says which (`ctrl+t expands`, `ctrl+t collapses`).
- With more than one live box, `Ctrl+P` moves focus to them as it does to panels, and `Enter` opens or closes the focused one.
- An open box in the live region shows at most `tui.tool_detail` rows of detail; the rest is in the viewer.

**When the round's last call ends,** the box and its detail commit together, as they are, and never change after. A closed box with detail behind it says `ctrl+o expands` in its bottom border, dim. Calls that finish early wait for the round: their detail appears when the round commits, and until then the open box shows it in the live region.

### The live region

```
          return Err(self.error("digit separator mu

╭────────────────────────────────────────────────────────────────────────────╮
│ ◆ also run clippy when that's done                                         │
╰────────────────────────────────────────────────────────────────────────────╯
  ⣾ 14s  esc stops · enter steers this turn · tab queues it      opus-5.5 high · fast
```

From top to bottom:

1. **Streaming tail.** The unfinished last line or lines of the current block, or the two reasoning rows.
2. **The running round.** Its box, with the calls' lines and any open detail.
3. **Panels.** Whatever currently has something to show ("Panels", below).
4. **Dialog.** An approval or question replaces the input box while it is open.
5. **Input box.** A rounded box with a dim border, holding `◆` and the text. It grows to 8 lines, then scrolls inside itself. When it is empty it shows a dim placeholder.
6. **Hint row.** One dim row under the input box, in place of a status line. Its left side holds the keys that matter now, and its right side the status that matters now.

The hint row's left side:

| When | Shows |
|---|---|
| A turn is running | The spinner and the turn's elapsed time, then `esc stops · enter steers this turn · tab queues it instead`. Steering and queueing are offered only while a turn runs. |
| A dialog, a menu or a focused panel | Its keys. |
| Idle, after a turn | The turn's cost and the session's, for a while. |
| Idle, in the first sessions | `/ commands · @ files · alt+enter new line`. |

The hint row's right side:

| Segment | Shown |
|---|---|
| Model | Always: the short model name and the reasoning level. |
| Speed | When not `standard`: `fast` in the accent, and a cooldown while one runs ([decision 0027](../decisions/0027-speed-tiers.md)). |
| Context | From 50% of the context window: the percentage, in the warning colour from 70% and the error colour from 90%. |
| Cost | When a budget is set: the spend against it ([14-adaptation.md](14-adaptation.md)). Otherwise on the left, for a while after each turn. When a provider reports a limit and less than a fifth of it is left: what remains and when it resets. |
| Access | When it is not the default: `sandbox off` or `unrestricted` in the warning colour, a command that ran outside the sandbox, and under a task lease (after the first release) what remains of its time and budget. |
| Extras | When relevant: the queued prompt count, MCP or extension problems, a rate-limit wait. |

- **Narrow windows.** Segments are dropped from the right when the window is narrow. Under 50 columns the hint row shows the model and the context only.
- **Everything else is a key away:** `/usage` for cost and the cache hit rate, `/context` for what the window holds, `/status` for the rest.
- **Height.** While an answer streams, the live region is 6 rows: the tail, a blank row, the 3 rows of the input box, and the hint row. A running round adds its 2 border rows, one row per call, and any open detail. Rows that do not change (the box's borders) cost nothing on a frame that commits nothing.

### Panels

A panel is a bounded block in the live region that appears when it has something to show and disappears when it does not. The chat flow stays the default view; nothing is permanently split off.

| Panel | Opens when | Shows | Closes when |
|---|---|---|---|
| **Plan** | The `plan` tool has items. | `◇ Plan  1 of 4 done`. Collapsed: that line with the active item after it. Expanded: one item per line, two columns in, with `☐`, `◧` (active, in the accent), `☑` and `⊘` (skipped). | The plan is empty or all items are done and the turn has ended. |
| **Review** | A turn ended having changed files. | One line per changed file with `+` and `−` counts, and the verification result. | The next prompt is sent. The changes stay reachable through `/view` and `/undo`. |
| **Usage** | `/usage`, or a limit is near. | This session: tokens by class, cache hit rate, cost by model, speed. History: usage and cost per provider and model for today, the week, the month and all time, with a breakdown per project, and what is left of each provider's limits ([18-usage.md](18-usage.md)). Near a limit and not opened: one line in the warning colour. | Dismissed, or the limit is no longer near. |
| **Project** | Learned entries are waiting for review, or `/project`. | Proposed commands, conventions and facts with their sources. | Reviewed or dismissed. |
| **Attempts** (after the first release) | Parallel attempts are running. | One row per attempt: variant, state, cost, checks. | A winner is picked or the attempts are discarded. |
| **Employees** (after the first release) | A Coordinator team is running. | One row per Employee: task, state (working, waiting, needs approval, stuck, done), model, spend, last activity. `Enter` attaches to that Employee: the transcript view switches to its session, and a banner shows whose session it is and how to return. | The team has finished and its work is integrated or discarded. |
| **Escalations** (after the first release) | Something is waiting for the user's decision. | The queue: who is asking, what kind of decision, the question. `Enter` opens the detail (command, diff, plan) with the choices. A count shows in the hint row. | The queue is empty. |

- **Height.** Each panel is collapsed by default to one to three lines. The live region's overall limit still applies; when several panels compete, the oldest collapses first.
- **Focus.** `Ctrl+P` moves focus from the input to the live tool boxes and the panels, and back. In a focused panel, `Up` and `Down` move, `Enter` expands an item or opens it in the viewer, and `Esc` returns to the input. The focused item is drawn in the accent, in bold.
- **Editing the plan.** With the plan panel focused: `Space` skips or restores an item, `e` rewords it, `Alt+Up` and `Alt+Down` reorder, `a` adds, `d` deletes. Each change takes effect at once and is passed to the agent at its next safe boundary ([05-tools.md](05-tools.md)).
- **In the full-screen presenter**, on a wide window, panels may sit beside the transcript instead of above the input. The components are the same. The owner chose stacked panels as the default on 2026-11-12.

### Dialogs and menus

Dialogs appear in the live region, above the hint row, in place of the input box.

**Approval.**

```
? Edit .github/workflows/ci.yml?
  CI configuration is protected, so a change to it always asks.

     29         - uses: dtolnay/rust-toolchain@stable
     30 −         with: { components: rustfmt }
     30 +         with: { components: rustfmt, clippy }
     31         - run: cargo test --workspace
     32 +       - run: cargo clippy --workspace -- -D warnings

  y Allow once   s Allow this session   a Always allow here   n Deny   e Deny with a note

  esc denies · ctrl+o shows it whole                                  opus-5.5 high · fast
```

- **The question** is bold, after a `?` in the warning colour. A dim line says why it is asked.
- **The detail** shows exactly what will run, wrapped, never shortened, or the diff with the line-number and sign gutter. Control characters are drawn as visible symbols. A diff is clipped to the live region, and `Ctrl+O` opens it whole.
- **The answers** are on one line, each key in the accent. Letters choose directly, and `Esc` denies. A choice is offered only where [06-permissions-and-sandbox.md](06-permissions-and-sandbox.md) offers it.
- **Leaving the sandbox** asks `Run outside the sandbox?`. It shows the command and its folder, and says in the warning colour that it would run with the user's full rights and that this is asked every time. It offers only `y Run once outside the sandbox` and `n Deny`.

**Menus** (slash commands, file completion, model and session pickers) open below the input box, in place of the hint row. They show up to 8 rows, filtered as the user types. The selected row is marked with `◆` and drawn in the accent, in bold. `Tab` or `Enter` accepts, and `Esc` closes. The `/effort` menu ends with its fast-mode row, `tab - fast mode` and its state, when the model offers a fast speed.

### The viewer

`Ctrl+O`, or `/view`, opens the session in a temporary full-screen view on the alternate screen.

- **Layout.** A title row (`Viewer`, the folder, the session and its turns, and `esc returns to the session`), then the transcript, then a key row.
- **Boxes.** Every tool box starts closed, and the latest is selected, with its border in the accent. `Up` and `Down` move between boxes. `Enter` opens or closes the selected one with all its detail: the whole diff and the whole output, not a tail.
- **Moving and searching.** `PageUp` and `PageDown` scroll, and `/` searches.
- **Returning.** `Esc` returns to the inline screen exactly as it was.
- **Other long content.** The viewer also opens the reasoning summary, a compaction summary, and an item named by `/view n`.

This is how inline mode offers "expand" without rewriting the scrollback.

### The mouse

**Not in inline mode.** A terminal reports the mouse for the whole window or not at all; no mode limits it to some rows.
- With reporting on, the wheel is sent to the program instead of scrolling the scrollback, and a drag no longer selects text (Windows Terminal and xterm then need `Shift`). That breaks the two things inline mode exists for.
- Reporting cannot be turned on only while the pointer is over the live region, because nothing is reported until it is on.
- tmux passes clicks only when its own mouse mode is on.
- The inline presenter never asks where the cursor is, so it does not know which screen row its live region starts on. Mapping a click to a box would need the cursor-position query that [decision 0002](../decisions/0002-tui-rendering.md) avoids.

Inline, the keys do the work: `Ctrl+T`, and `Ctrl+P` then `Enter`.

**In the viewer and the full-screen presenter,** which own the screen, mouse reporting is on while they are shown (modes 1000 and 1006):
- a click on a box opens or closes it;
- the wheel scrolls the transcript;
- `Shift` and a drag select text with the terminal's own selection.

It costs an SGR mouse decoder in `cortex-term`, about 150 lines with its tests. On Windows it also needs the console's mouse input turned on while the alternate screen is shown; the layer turns that input off today. Step 25 builds both. Until then, the viewer works by keys alone.

### Colour and glyphs

**Colours are semantic tokens.** The default theme uses one brand colour, the accent, and maps every other token to the terminal's own 16 colours, so the UI follows the user's terminal theme, light or dark.

**The accent is the logo's purple, `#7A3CFB`.** It marks your `◆` (in the transcript and the input), the spinner, `fast`, the active plan item, the answer keys in a dialog, the selected menu row, and the viewer's selected box. Nothing else.

| Terminal | Accent shown as |
|---|---|
| True colour | `#7A3CFB`. |
| 256 colours | Index 99, the nearest. |
| 16 colours | Named by the theme: bright magenta (13) on a dark background, magenta (5) on a light one. |

**The theme names its 16-colour accent itself.** The automatic reduction (`Color::for_depth`) brings `#7A3CFB` to the nearest xterm colour, which is bright blue (12), not magenta.

**On a dark background at 16 colours, the accent marks but does not carry text.** No magenta reaches 4.5 to 1 there: on Windows Terminal's default scheme (Campbell, `#0C0C0C`), magenta is 2.4 to 1 and bright magenta 3.2 to 1. There, accent-coloured text (the selected menu row, the focused plan item) is drawn bold in the default colour, and only glyphs take the magenta. On a light background magenta reaches 5.9 to 1 (One Half Light) and is used as it is.

**The tint for text.** In true colour, on a dark background, that purple has a contrast of about 3.6 to 1, enough for marks and not for text. Text in the accent colour therefore uses a lighter tint (`accent.text`), chosen to reach at least 4.5 to 1 against the terminal's background, which is queried at start-up: `#8D57FC`, 4.6 to 1, on Campbell. On a light background the base colour already reaches 5.2 to 1 and is used as it is. When the background cannot be determined, the dark-background tint is used.

| Token | Default | Used for |
|---|---|---|
| `text` | default foreground | Answers, and the answer's `◇`. |
| `muted` | bright black | Reasoning (italic), tool detail, notices, hints, line numbers, box borders and rules. |
| `accent` | the logo purple, as above | The marks listed above. |
| `accent.text` | a tint of the accent, as above | Accent-coloured text. |
| `user` | the accent | Your `◆`. Your message itself is bold, in the default colour. |
| `title` | bold default foreground | A tool call's imperative, a dialog's question. |
| `code` | bright cyan on a dark background; cyan and bold on a light one | Inline code. Bright cyan is 11.3 to 1 on Campbell. Cyan is 3.3 to 1 on One Half Light, under the target for text, so it is bold there. |
| `heading` | bold default foreground | Headings. |
| `success` | green; bright green on a dark background | `✓`, an edit's `+` count. |
| `warning` | yellow; bright yellow on a dark background | `?`, warnings, the context from 70%. |
| `error` | red; bright red on a dark background | `✗`, errors, a failure's measure, the context from 90%. |
| `diff.add`, `diff.del` | green and red; bright on a dark background | The sign of an added or removed line, and the whole line where there is no background colour. |
| `diff.add.bg`, `diff.del.bg` | in true colour, 20% of green (`#2EA043`) or red (`#E03E3E`) mixed into the background on a dark one, 13% on a light one; none otherwise | The tint of an added or removed line. The text on it keeps the default foreground: 9.1 to 10.2 to 1 on the two sample schemes. |
| `diff.emph` | bold | Changed words inside a changed line. |

- **The bright colours on a dark background.** The default theme takes success, warnings, errors and the diff signs from the bright half of the terminal's palette on a dark background, and from the normal half on a light one: Campbell's red is 3.2 to 1 on its background, and its bright red 5.1.
- **Elements are tokens too.** Each element is drawn in one of the tokens above, or in one with an attribute: a box's border is `muted`, a key in a dialog is `accent.text` and bold, an added line's sign is `diff.add` on `diff.add.bg`. `cortex_tui::theme::Token` lists them, and the theme alone decides how each looks at each depth and on each background, so no view holds a rule for 16 colours or for none.
- **Blue on a dark background.** Plain blue is not used for text there: Campbell's blue (`#0037DA`) is 2.4 to 1 on its background, and its bright blue 5.0 to 1. A theme that wants blue on a dark background uses bright blue.
- **Themes** are TOML tables that assign a colour to each token: an ANSI colour name, a 256-colour index or a hex value. `tui.theme` selects a built-in or user theme.
- **Without colour.** `NO_COLOR` and `--no-color` turn colour off. Meaning is never carried by colour alone: every state has a glyph or a word, diff lines keep their sign, removed lines are dim, and your `◆`, the answer's `◇`, the answer keys and the selection are bold.
- **Colour depth** is detected, and hex values are reduced when the terminal cannot show them.

**Glyphs.** `tui.glyphs = "auto"` uses the Unicode set when the locale is UTF-8, and the ASCII set otherwise (and on the old console host).

| Use | Unicode | ASCII |
|---|---|---|
| You, the input | `◆` | `>` |
| The answer | `◇` | `o` |
| Reasoning | `∴` | `~` |
| Succeeded, failed, waiting, denied, interrupted | `✓` `✗` `?` `⊘` `■` | `+` `x` `?` `-` `#` |
| Spinner | `⣾⣽⣻⢿⡿⣟⣯⣷` | `\|/-\` |
| Notice | `·` | `.` |
| Box corners and edges | `╭ ╮ ╰ ╯ ─ │` | `+ + + + - \|` |
| Plan items | `☐ ◧ ☑ ⊘` | `[ ] [>] [x] [-]` |
| A removed line's sign, an ellipsis, an arrow | `−` `…` `→` | `-` `...` `->` |
| Between the parts of a measure or a hint (`exit 101 · 3.4s`) | `·` | `\|` |
| A range of numbers (`lines 120–178`) | `–` | `-` |
| An unordered list item | `–` | `-` |
| The spinner under `tui.reduced_motion` | its first frame, still | its first frame, still |

Width is computed per grapheme, and the layout never depends on an ambiguous-width character being narrow. Whether each Unicode glyph is in Windows Terminal's default font is checked by eye with the reference screens in [tui-look/](tui-look/README.md).

**Density.** `tui.density = "comfortable"` (default) puts a blank line between blocks. `"compact"` puts one only between turns, and shows reasoning as its first line alone.

## Input

### The editor

A multi-line text editor of our own. A text-area crate was considered and not used: paste handling, attachments, completion menus and wrapping in a growing region are most of the work and are specific to this UI.

- **Text model.** UTF-8 with cursor movement, deletion and wrapping by grapheme cluster.
- **Movement and editing.** Characters, words, line ends, buffer ends. Delete word and line. One kill-and-yank slot. Undo and redo with typing grouped into words.
- **History.** Up and Down on the first and last line step through earlier prompts for this project. `Ctrl+R` searches them.
- **Paste.** Bracketed paste is enabled, so pasted text is inserted as text and never interpreted as keys. A paste of more than 12 lines or 2,000 characters is shown as a chip (`[pasted 148 lines]`) and expanded when the prompt is sent. A key expands it in place.
- **Files.** Typing `@` opens file completion over the workspace, with fuzzy matching and the ignore rules. An accepted path is inserted as a token and sent as a reference the agent can read.
- **Commands.** `/` at the start of the prompt opens the command menu.
- **External editor.** `Ctrl+G` opens the prompt in `$EDITOR` and reads it back.
- **While a turn is running** the editor stays usable. `Enter` sends the text to the running turn (steer). `Tab` on an empty completion context switches the pending message between "steer" and "queue", shown on the hint row. See [03-agent-loop.md](03-agent-loop.md).

### Default keys

Every binding can be changed in `[keys]`. Bindings are looked up through a key map; no key is tested for directly in component code.

| Key | Action |
|---|---|
| `Enter` | Send. In a menu: accept. On a focused live box: open or close it. |
| `Shift+Enter`, `Alt+Enter`, `Ctrl+J` | New line. Three keys because not every terminal can tell `Shift+Enter` from `Enter`. |
| `Esc` | Close the menu or dialog. Otherwise interrupt the running turn. |
| `Ctrl+C` | Interrupt the running turn. If idle: clear the editor. Twice when idle and empty: quit. |
| `Ctrl+D` | Quit when the editor is empty. |
| `Ctrl+L` | Redraw the live region. |
| `Ctrl+O` | Open the viewer at the latest tool box. |
| `Ctrl+P` | Move focus between the input, the live tool boxes and the panels. |
| `Ctrl+T` | Open or close the running round's box in place. |
| `Ctrl+R` | Search prompt history. |
| `Ctrl+G` | Edit the prompt in the external editor. |
| `Tab` | Accept a completion. With no completion and a turn running: switch steer and queue. |
| `Up`, `Down` | Move in the editor. At its first or last line: history. In a menu: move. |
| `Ctrl+A`, `Ctrl+E`, `Ctrl+W`, `Ctrl+K`, `Ctrl+U`, `Ctrl+Y`, `Alt+B`, `Alt+F`, `Ctrl+Z`, `Ctrl+Shift+Z` | The usual line-editing actions: line start and end, delete word, delete to end, delete to start, yank, word left and right, undo, redo. |
| `PageUp`, `PageDown` | Full-screen presenter: scroll. Inline: left to the terminal. |

### Slash commands

| Command | Does |
|---|---|
| `/help` | Lists commands and keys. |
| `/new` | Starts a new session. |
| `/sessions` | Opens the session picker: resume, switch, fork. |
| `/rewind` | Picks an earlier point to continue from, with or without restoring files. |
| `/undo` | Restores the files changed by the last turn and steps back before it. |
| `/model`, `/effort` | Pickers for the model and reasoning level. When the current model offers a fast speed ([04-providers.md](04-providers.md), "Speed tiers"), the `/effort` picker also has a fast-mode toggle on `Tab`, with the hint `tab - fast mode`. When the model offers none, neither the toggle nor the hint is shown. The owner asked for this on 2026-11-12 ([decision 0027](../decisions/0027-speed-tiers.md)). In that picker `Enter` accepts the level, because `Tab` is the toggle. |
| `/mode` | Picks the level (the permission mode). |
| `/lease` | After the first release: grants, shows, extends or ends a task lease ([06-permissions-and-sandbox.md](06-permissions-and-sandbox.md)). |
| `/permissions` | Lists and removes stored approvals. |
| `/compact [focus]` | Compacts now. |
| `/context` | Shows what the context window holds, by category. After the first release it opens the context profiler ([15-efficiency.md](15-efficiency.md)). |
| `/usage` | Opens the usage panel: this session, and usage and cost by provider, model and project over time, with limits left ([18-usage.md](18-usage.md)). |
| `/plan` | Focuses the plan panel. |
| `/project` | Opens the project profile and memory for review. |
| `/profile <name>` | Switches the settings profile. |
| `/verify` | Runs the project's checks now. |
| `/reload` | Reloads configuration, extensions, themes and key bindings. |
| `/extend <request>` | Asks the harness to write a new tool or command as an extension ([12-extensions.md](12-extensions.md)). |
| `/status` | Provider, limits, sandbox, MCP servers, extensions. |
| `/view [n]` | Opens an item in the viewer. |
| `/copy` | Copies the last answer to the clipboard through the terminal. |
| `/config` | Shows the effective configuration and where each value comes from. |
| `/theme` | Picks a theme. |
| `/mcp`, `/extensions` | Status and management. |
| `/export` | Writes the session as Markdown or JSON, redacted. |
| `/quit` | Exits. |

Custom commands (markdown files, see [12-extensions.md](12-extensions.md)) and extension commands appear in the same menu. Extension commands are prefixed with the extension's name.

## Terminal layer (`cortex-term`)

- **No terminal library** ([decision 0002](../decisions/0002-tui-rendering.md), "Amended in step 20"). Raw mode, the window's size, waiting for input and the handlers that put the terminal back are a module for each system (`sys::unix` on `libc`, `sys::windows` on `windows-sys`), the only `unsafe` code in the crate, behind a small trait (`Device`) so that they can be replaced and so that the layer's own logic is tested without a terminal. Input is decoded by our own decoder, the same on both systems. Output is written by our renderer, one write a frame.
- **Start-up.** The first frame is drawn at once with conservative assumptions. Capability queries go out just before it, when the terminal is entered, and their replies upgrade behaviour in later frames. Start-up never waits for a terminal to answer.
- **Capabilities, each detected and each with an override in `[terminal]`:**

| Capability | Detection | Fallback |
|---|---|---|
| Synchronized updates | Mode report query. | Emit anyway unless the terminal is on a small deny-list; terminals ignore private modes they do not know. |
| Enhanced keyboard reporting | Protocol query. | `Alt+Enter` and `Ctrl+J` for new lines. |
| Colour depth | Environment variables. | 16 colours. |
| Hyperlinks | Known-terminal list. | Plain text with the URL. |
| Clipboard through the terminal | Assumed; `/copy` reports when it cannot tell. | The text is shown for manual copying. |
| Bracketed paste, focus events | Requested; harmless when unsupported. | Pastes arrive as keys and are rate-detected. |
| Unicode | Locale. | ASCII glyphs. |

- **Restoring the terminal.** Raw mode, cursor visibility, keyboard mode, paste mode and the alternate screen are restored on normal exit, on a panic, and on termination signals.
- **Suspend and resume** (`Ctrl+Z` on Linux) restore the terminal on suspend; on resume, raw mode, bracketed paste, focus events, automatic wrap off and whichever keyboard protocol was on come back, and the live region is drawn again from where the cursor is.
- **After the terminal has been put back in an emergency** (a panic on another thread, a signal), the layer writes nothing more: a frame would turn modes on again in a terminal that is no longer the program's. The program's next write fails, and it should end. The signal handlers replace whatever was installed for those signals before; a graceful shutdown on SIGTERM is the interactive application's to design ([outstanding/signals-and-a-graceful-shutdown.md](../outstanding/signals-and-a-graceful-shutdown.md)).
- **Automatic wrap is off while the program runs.** A character the terminal draws wider than it is counted (an emoji sequence, an ambiguous-width character in a CJK locale) cannot then push a row onto the next and throw the presenter's count of rows out; at worst the row is cut short by the terminal.
- **Sanitising.** All text from models, tools and files passes through one function (`cortex_term::sanitize`) that makes visible every control character except newline and tab. Only the renderer emits control sequences, and it shows every control character that reaches it in a line, newline and tab included, as a symbol one column wide: C0 characters as their Control Pictures (`␛`), DEL as `␡`, and C1 characters (`U+0080` to `U+009F`, which some terminals obey like `ESC [`) as `�`. A link's address is used only when it is printable ASCII.

### The terminal layer as built (step 20)

What steps 21 to 25 build on. Its public items are documented in the crate.

| Part | What it is |
|---|---|
| `Line`, `Span`, `Style`, `Color` | The styled-line model. A span is text, a style (two colours, bold, dim, italic, underline, reverse) and perhaps a link. A colour is brought down to what the terminal shows: true colour, 256, 16, or none (`NO_COLOR`). Nothing here names a colour, glyph or layout. |
| `Inline` | The inline presenter. `draw(&Frame { commit, live, cursor }, &caps, &mut sink)` writes one frame in one call to a `Sink`, or nothing when the screen would not change. `resize`, `redraw` (Ctrl+L), `restart` (after a suspension), `finish` (erase the live region before exit), `width` (the columns a line may use). |
| `Scheduler` | When to draw: `input()` draws at once, `stream()` at most 60 times a second, `animate(every)` ticks, and `wake(now)` says `Now`, `At(time)` or `Idle`. It holds no clock: the loop gives it the time. |
| `Terminal` | Raw mode on `enter`, the start sequences (bracketed paste, focus events) and the capability queries, then `next(timeout)` for events, and `leave` (or dropping it) to put everything back. It is the `Sink` a presenter writes to. `waker()` wakes `next` from another thread; `suspend()` is Ctrl+Z. |
| `Decoder`, `Event`, `Key` | Keys with modifiers, pastes (one event, whatever they hold), focus changes, replies to queries, resizes and resumptions. The same bytes decode the same way however reads split them. |
| `Capabilities`, `Overrides` | Detected from the environment, upgraded by replies, overridden by settings. The `[terminal]` section that holds the overrides is step 23's to declare; the layer takes a plain `Overrides`. |
| `sanitize` | The one function content passes through. |

**What the design round must respect.** The presenter draws rows of styled text, one row a line, and nothing else: no cell is addressed on its own, no region scrolls, and nothing above the live region can change once committed. What is cheap: a live region of a few rows, rewritten every frame; changing one row (only the changed rows are written when nothing was committed and the height is the same); colour, the five attributes and links on any span. What costs more: a tall live region (30 rows measured at 0.14 to 0.5 ms a frame and about 4 KB, the size at which a frame no longer fits one read of a pseudo-terminal), and anything that changes the region's height every frame, which redraws all of it. What it cannot do: draw in the last column, re-wrap or restyle anything already committed, place things side by side except as text laid out in a row, or keep a region taller than the screen.

### Windows

| Topic | Handling |
|---|---|
| Console set-up | UTF-8 code pages, virtual-terminal processing for output, and virtual-terminal input are turned on at start; line input, echo and Ctrl+C as a signal are turned off. The modes and code pages found are put back on a normal exit, a panic and a console control event (Ctrl+Break, closing the window, logging off, shutting down). A console that refuses virtual-terminal processing, as consoles before Windows 10 do, is refused with a message. |
| Key events | With virtual-terminal input, keys arrive as the same sequences a Linux terminal sends, and one decoder reads both. Key releases are not reported in that mode; a release that the enhanced keyboard protocol reports is dropped. What the mode cannot tell apart is in the next row. |
| Keys the mode merges | Plain virtual-terminal input cannot tell `Shift+Enter` from `Enter`, `Ctrl+Enter` from `Ctrl+J`, `Ctrl+Tab` from `Tab`, or `Ctrl+Shift` with a letter from `Ctrl` with it, and a lone `Esc` waits 50 ms for the rest of a sequence before it is the Escape key. Two things win them back: the enhanced keyboard protocol, where the terminal has it, and Windows Terminal's **win32 input mode** (mode 9001), which reports every key as a console input record in a sequence of its own (`CSI Vk ; Sc ; Uc ; Kd ; Cs ; Rc _`). The layer asks for both as it starts and turns on whichever the terminal confirms; the same decoder reads them. Where neither is there (the old console host), a new line is `Alt+Enter` or `Ctrl+J`. A terminal library that read console records (crossterm) would have had `Shift+Enter` in every console host; that is the price of one decoder and no library ([decision 0002](../decisions/0002-tui-rendering.md), "Amended in step 20"). **What is checked, and how:** the decoder's unit tests check every key above by its sequences, plain and win32. The system tests check under a hosted runner's ConPTY that keys sent as Windows Terminal sends them, win32 input records, reach the demo with every modifier: `Shift+Enter`, `Ctrl+Enter`, `Ctrl+Tab`, `Ctrl+Shift+A`, `Alt+X`, `Ctrl+Space`, `Ctrl+Up` and `Escape`, and that a lone Escape waits and is the Escape key. None is checked in a real Windows Terminal window; that is for the owner, by eye, with the demo. |
| `Shift+Enter` | Not distinguishable in every Windows terminal. `Alt+Enter` (where the terminal does not reserve it) and `Ctrl+J` always work. |
| Ctrl+C | Read as a key in raw mode, not as a signal, so a turn can be interrupted without ending the process. |
| Legacy console host | Taken to be the terminal when none of `WT_SESSION`, `TERM_PROGRAM` and `TERM` is set: no synchronized updates, ASCII glyphs, 16 colours. The checklist found it printing every escape sequence as text because nothing turned virtual-terminal processing on; the terminal layer now does. The system tests run the demo with that mode turned off first, as the old console host has it, under ConPTY (which is the console host without its window), and the demo draws correctly. A real window of the old console host is not seen by CI: whether it draws correctly now is for the owner to check by eye. Windows Terminal is the supported target. |
| Inline drawing under the console layer | Confirmed in Windows Terminal 1.24 by the manual checklist in [../measurements/windows-terminal-checklist.md](../measurements/windows-terminal-checklist.md) ([decision 0002](../decisions/0002-tui-rendering.md), "Checked on Windows"). If inline drawing misbehaves in a terminal, the full-screen presenter is the fallback. |

## Structure of `cortex-tui`

```
events from core ─► state ─► view (pure: state → styled lines) ─► presenter ─► terminal
keys ─► key map ─► actions ─► state changes and Ops to core
```

- **State** is plain data: the items of the active branch (for the full-screen presenter and the viewer), the live item, editor state, dialog state, status values.
- **View functions are pure.** Given state, width and theme they return lines. They are what the snapshot tests cover.
- **Actions** are an enum. Keys and slash commands both produce actions, so everything is reachable both ways and testable without a terminal.
- The TUI holds no agent logic. It never decides what a tool may do, never touches files, and never talks to a provider. It displays events and sends operations.

## Testing

Covered in [../testing-and-ci.md](../testing-and-ci.md): snapshot tests of view functions, presenter tests against a virtual terminal that assert the no-flicker properties, and end-to-end tests of the real binary under a pseudo-terminal on Linux and Windows.

## Not in the first release

Mouse support in inline mode ("The mouse", above; the viewer and the full-screen presenter have it from step 25), image display, a vi editing mode, screen-reader mode, and running the user's own shell commands from the prompt. None is ruled out by the design.

Planned for after the release, each with its own build step: task leases and `/lease`, the attempts panel, the Employees and escalations panels, the full context profiler behind `/context`, and other tools' records in `/usage`.
