# The terminal UI

The interactive front end. It is written in Rust and lives in the main binary, in two crates: `cortex-term` (terminal layer and renderer, no knowledge of agents) and `cortex-tui` (the application). The language choice is [decision 0001](../decisions/0001-tui-language.md) and the rendering approach is [decision 0002](../decisions/0002-tui-rendering.md). The owner accepted both on 2026-11-12, and both were confirmed on 2026-11-13 by the Windows Terminal checklist.

The look described here is our own. It was designed from the requirements, not from another tool's screen. Glyphs, wording, colours, commands and default keys are defaults. The owner accepted them as drafted on 2026-11-12; the TUI build steps implement what this page says and do not need to invent more.

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
 │  running tools with a short output tail                                    │
 │  task list, approval dialog or completion menu, when present               │
 │  ─────────────────────────────────────────────────────────────────────     │
 │  › the editor                                                              │
 │  status line                                                               │
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
| Reasoning | Not committed while streaming. Replaced by one summary line when it ends. |
| Tool call | A running tool is live. Its final chip, and any output or diff shown with it, is committed when it completes. |
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

**Live region height.** At most half the window, and at least the editor and the status line. Content that would exceed it is clipped with a marker (for example a tool's output tail shows fewer lines). The presenter itself only refuses a region taller than the screen: it keeps the region's bottom rows, because rows that had scrolled away could not be reached again.

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

Widths below assume 80 columns or more. Text wraps at the window width or `tui.max_width` (default 110), whichever is smaller.

### A session, as it appears in the scrollback

```
▌ fix the failing lexer test and explain what was wrong

∴ thought for 6s
The test fails because `scan_number` stops at the first underscore, so
`1_000` is read as `1`. I'll check how digits are consumed.

  ⏵ search  scan_number in crates/parser                    ✓ 3 matches
  ⏵ read    crates/parser/src/lex.rs  120–178               ✓ 59 lines
  ⏵ edit    crates/parser/src/lex.rs                        ✓ +4 −1
    @@ scan_number
    141    while let Some(c) = self.peek() {
    142 -      if !c.is_ascii_digit() { break; }
    142 +      if c == '_' { self.bump(); continue; }
    143 +      if !c.is_ascii_digit() { break; }
    144        self.bump();
  ⏵ shell   cargo test -p parser                            ✓ 2.1s

All 48 tests pass now. The loop treated `_` as the end of the number;
it now skips separators inside digit runs.
```

### Elements

| Element | Appearance | Notes |
|---|---|---|
| **User prompt** | Every line starts with a bar `▌` in the user colour, then the text. One blank line before and after. | The bar makes prompts easy to find when scrolling back. |
| **Assistant text** | Flush left, default colour, no decoration. Blank line between blocks. | Selecting and copying gives clean text. |
| **Code block** | Flush left, highlighted when the feature is on. A dim line above carries the language name. No border and no background by default. | Copying gives the code and nothing else. |
| **Inline code** | The `code` colour. | |
| **Headings** | Bold, with the heading colour. No `#`. | |
| **Lists, quotes** | Standard markers (`•`, numbers), quotes with a dim `│`. | |
| **Tables** | Columns aligned with spaces, a dim rule under the header. | Falls back to stacked `key: value` rows when too wide. |
| **Links** | Underlined text, as a terminal hyperlink where supported, otherwise followed by the URL in dim. | |
| **Reasoning** | While streaming: one live line, dim italic, showing the latest words. When finished: one dim line, `∴ thought for 6s`. | `tui.reasoning = "summary"` prints the provider's summary, dim and indented, instead of the one-liner. |
| **Tool chip** | Indented two columns: `⏵`, the tool name in the tool colour, a summary of the arguments, and at the right edge a state mark with a measure. | One line per call. |
| **Tool output** | Indented four columns, dim. Shown under the chip for failed commands (last 8 lines) and not shown for successful ones. | `tui.tool_output = "auto"`, `"always"` or `"never"`. |
| **Diff** | Under an edit or write chip, indented four columns: a hunk header, then lines with a line-number column (dim), a sign column, and the text. Removed lines in the removal colour, added lines in the addition colour, with the changed words emphasised. Up to 40 lines, then `… 120 more lines · Ctrl+O to view`. | The line-number column is dropped under 100 columns. |
| **Notice** | One dim line starting with `·`. | Retries, compaction, sandbox reports, instruction changes. |
| **Error** | A line starting with `✗` in the error colour, then an indented dim line with what to do next. | |
| **Turn summary** | Optional dim line after a turn: duration, tokens and cost. | Off by default. `tui.turn_summary = true`. |

**Tool chip states.**

| Mark | Meaning | Right-hand text |
|---|---|---|
| spinner | running | elapsed time |
| `?` | waiting for approval | "approval needed" |
| `✓` | succeeded | a measure: lines, matches, `+4 −1`, duration |
| `✗` | failed | exit code or a short reason |
| `⊘` | denied | |
| `■` | interrupted | |

The spinner is an eight-frame braille cycle at 100 ms. `tui.reduced_motion = true` replaces it with a static mark.

### The live region

```
The loop treated `_` as the end of the number; it now skips separ▏
  ⏵ shell   cargo test -p parser                                  ⣽ 1.4s
      Compiling parser v0.3.0
      Running unittests src/lib.rs
  ☐ update the changelog   ☑ fix scan_number   ☑ add a test
──────────────────────────────────────────────── esc interrupt · enter steer ──
› also run clippy when that's done▏
 opus-5.5 · high    ▰▰▰▱▱▱▱▱▱▱ 31% · 62k    $0.42  supervised · sandboxed
```

From top to bottom:

1. **Streaming tail.** The unfinished last line or lines of the current block.
2. **Running tools.** A chip per running tool. A shell command shows the last 4 lines of its output.
3. **Panels.** Whatever currently has something to show (next section). In the picture: the plan, collapsed to one line.
4. **Rule.** A dim line. Its right end shows the keys that matter right now. While a turn is running it shows how a typed message will be delivered.
5. **Editor.** Starts with `›`. Grows to 8 lines, then scrolls inside itself. When empty it shows a dim hint.
6. **Status line.**

| Segment | Content |
|---|---|
| Model | Short model name and reasoning level. |
| Context | A ten-cell gauge, the percentage of the context window in use, and the token count. The gauge takes the warning colour at 70% and the error colour at 90%. |
| Cost | Session cost, when prices are known, and the cache hit rate of the last request (`cache 94%`). When a provider reports a limit and less than a fifth of it is left, what remains and when it resets. |
| Access | The level and sandbox state: `sandboxed`, `sandbox off`, or `unrestricted` in the warning colour. Under a task lease (after the first release), what remains of its time and budget. |
| Extras | Shown when relevant: queued prompt count, MCP or extension problems, rate-limit wait. |

Segments are dropped from the right when the window is narrow. Under 50 columns the status line shows the model and the gauge only.

### Panels

A panel is a bounded block in the live region that appears when it has something to show and disappears when it does not. The chat flow stays the default view; nothing is permanently split off.

| Panel | Opens when | Shows | Closes when |
|---|---|---|---|
| **Plan** | The `plan` tool has items. | Collapsed: one line of items with `☐`, `◧` (active), `☑`, `⊘` (skipped). Expanded: one item per line. | The plan is empty or all items are done and the turn has ended. |
| **Review** | A turn ended having changed files. | One line per changed file with `+` and `−` counts, and the verification result. | The next prompt is sent. The changes stay reachable through `/view` and `/undo`. |
| **Usage** | `/usage`, or a limit is near. | This session: tokens by class, cache hit rate, cost by model, speed. History: usage and cost per provider and model for today, the week, the month and all time, with a breakdown per project, and what is left of each provider's limits ([18-usage.md](18-usage.md)). | Dismissed, or the limit is no longer near. |
| **Project** | Learned entries are waiting for review, or `/project`. | Proposed commands, conventions and facts with their sources. | Reviewed or dismissed. |
| **Attempts** (after the first release) | Parallel attempts are running. | One row per attempt: variant, state, cost, checks. | A winner is picked or the attempts are discarded. |
| **Employees** (after the first release) | A Coordinator team is running. | One row per Employee: task, state (working, waiting, needs approval, stuck, done), model, spend, last activity. `Enter` attaches to that Employee: the transcript view switches to its session, and a banner shows whose session it is and how to return. | The team has finished and its work is integrated or discarded. |
| **Escalations** (after the first release) | Something is waiting for the user's decision. | The queue: who is asking, what kind of decision, the question. `Enter` opens the detail (command, diff, plan) with the choices. A count shows in the status line. | The queue is empty. |

- **Height.** Each panel is collapsed by default to one to three lines. The live region's overall limit still applies; when several panels compete, the oldest collapses first.
- **Focus.** `Ctrl+P` moves focus from the editor to the panels and back. In a focused panel, `Up` and `Down` move, `Enter` expands or opens the item in the viewer, and `Esc` returns to the editor.
- **Editing the plan.** With the plan panel focused: `Space` skips or restores an item, `e` rewords it, `Alt+Up` and `Alt+Down` reorder, `a` adds, `d` deletes. Each change takes effect at once and is passed to the agent at its next safe boundary ([05-tools.md](05-tools.md)).
- **In the full-screen presenter**, on a wide window, panels may sit beside the transcript instead of above the editor. The components are the same. The owner chose stacked panels as the default on 2026-11-12.

### Dialogs and menus

They appear inside the live region, between the task list and the rule. While a dialog is open the editor is replaced by the dialog's own input.

**Approval.**

```
  Allow this command?                                    shell · outside sandbox
    cargo publish --dry-run
    in crates/parser
  y once   s this session   a always here   n deny   e deny with note   v view
```

- The detail lines show exactly what will run, wrapped, never shortened. Control characters are drawn as visible symbols.
- For edits the dialog shows the diff, clipped to the live region, with `v` opening the viewer.
- Letters choose directly. `Esc` denies.

**Menus** (slash commands, file completion, model and session pickers) open below the editor and replace the status line, up to 8 rows, filtered as the user types. The selected row is reversed. `Tab` or `Enter` accepts, `Esc` closes.

**The viewer.** `Ctrl+O`, or `/view`, opens long content in a temporary full-screen view with scrolling and search: a tool's full output, a whole diff, the reasoning summary, a compaction summary. Closing it returns to the session exactly as it was. This is how inline mode offers "expand" without rewriting the scrollback.

### Colour and glyphs

**Colours are semantic tokens.** The default theme uses one brand colour, the accent, and maps every other token to the terminal's own 16 colours, so the UI follows the user's terminal theme, light or dark.

**The accent is the logo's purple, `#7A3CFB`.**

| Terminal | Accent shown as |
|---|---|
| True colour | `#7A3CFB` for marks, rules, the gauge and selection backgrounds. |
| 256 colours | Index 99, the nearest. |
| 16 colours | Magenta. |

On a dark background that purple has a contrast of about 3.9 to 1, enough for marks and not for running text. Text in the accent colour therefore uses a lighter tint (`accent.text`), chosen to reach at least 4.5 to 1 against the terminal's background, which is queried at start-up; on a light background the base colour already reaches 5.4 to 1 and is used as it is. When the background cannot be determined, the dark-background tint is used.

| Token | Default | Used for |
|---|---|---|
| `text` | default foreground | Assistant text. |
| `muted` | bright black | Notices, hints, line numbers, the rule. |
| `accent` | the logo purple, as above | The editor mark, the prompt bar, selections, the active plan item, the gauge. |
| `accent.text` | a tint of the accent, as above | Accent-coloured text. |
| `user` | the accent | The prompt bar. |
| `tool` | blue | Tool names. |
| `code` | yellow | Inline code. |
| `heading` | bold default foreground | Headings. |
| `success` | green | Success marks. |
| `warning` | yellow | Warnings, the gauge at 70%. |
| `error` | red | Errors, the gauge at 90%. |
| `diff.add` | green | Added lines. |
| `diff.del` | red | Removed lines. |
| `diff.emph` | bold | Changed words inside a changed line. |

- Themes are TOML tables that assign a colour to each token: an ANSI colour name, a 256-colour index or a hex value. `tui.theme` selects a built-in or user theme.
- `NO_COLOR` and `--no-color` turn colour off. Meaning is never carried by colour alone: every state also has a glyph or a word.
- Colour depth is detected and hex values are reduced when the terminal cannot show them.

**Glyphs.** `tui.glyphs = "auto"` uses the Unicode set above when the locale is UTF-8, and an ASCII set otherwise (`|`, `>`, `*`, `[x]`, `ok`, `!!`). Width is computed per grapheme, and the layout never depends on an ambiguous-width character being narrow.

**Density.** `tui.density = "comfortable"` (default) puts a blank line between items. `"compact"` does not.

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
- **While a turn is running** the editor stays usable. `Enter` sends the text to the running turn (steer). `Tab` on an empty completion context switches the pending message between "steer" and "queue", shown on the rule. See [03-agent-loop.md](03-agent-loop.md).

### Default keys

Every binding can be changed in `[keys]`. Bindings are looked up through a key map; no key is tested for directly in component code.

| Key | Action |
|---|---|
| `Enter` | Send. In a menu: accept. |
| `Shift+Enter`, `Alt+Enter`, `Ctrl+J` | New line. Three keys because not every terminal can tell `Shift+Enter` from `Enter`. |
| `Esc` | Close the menu or dialog. Otherwise interrupt the running turn. |
| `Ctrl+C` | Interrupt the running turn. If idle: clear the editor. Twice when idle and empty: quit. |
| `Ctrl+D` | Quit when the editor is empty. |
| `Ctrl+L` | Redraw the live region. |
| `Ctrl+O` | Open the most recent long output in the viewer. |
| `Ctrl+P` | Move focus between the editor and the panels. |
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
- **Start-up.** The first frame is drawn at once with conservative assumptions. Capability queries go out with it, and their replies upgrade behaviour in later frames. Start-up never waits for a terminal to answer.
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
- **Suspend and resume** (`Ctrl+Z` on Linux) restore the terminal on suspend and redraw on resume.
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
| Keys the mode merges | `Shift+Enter` arrives as `Enter`, `Ctrl+Enter` as `Ctrl+J`, `Ctrl+Tab` as `Tab`, and `Ctrl+Shift` with a letter as `Ctrl` with it, unless the terminal has the enhanced keyboard protocol. A lone `Esc` waits 50 ms for the rest of a sequence before it is the Escape key. Windows Terminal's own way to report every key with its modifiers (its "win32 input mode") is not used yet. |
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

Mouse support in inline mode, image display, a vi editing mode, screen-reader mode, and running the user's own shell commands from the prompt. None is ruled out by the design.

Planned for after the release, each with its own build step: task leases and `/lease`, the attempts panel, the Employees and escalations panels, the full context profiler behind `/context`, and other tools' records in `/usage`.
