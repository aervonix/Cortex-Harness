# 0033: The TUI's look is "Roomy 2": glyph-led blocks, answers apart, tool rounds in boxes that expand

- **Status:** Chosen by the owner on 2026-11-14, after three rounds of proposals. Replaces the draft that the owner accepted on 2026-11-12 (owner decision 4 in [build-plan.md](../build-plan.md)).
- **Date:** 2026-11-14

## Context

The owner accepted 09-tui's drafted look on 2026-11-12, then reopened it so that the look could be chosen from real alternatives before steps 21 to 24 build anything visible. A design round ran before step 21. Each proposal covered the same things:

- the transcript, with user, assistant and tool blocks, streaming markdown with a code block, a diff, reasoning, notices and errors;
- the editor;
- the status;
- the `/model` and `/effort` pickers, with the fast-mode toggle;
- an approval, and a request to leave the sandbox;
- the stacked panels;
- the accent and its tint;
- glyphs and density.

The rules were fixed. Every proposal had to be our own work (rule 1), and drawable by the inline presenter as step 20 built it: rows of styled text only, the last column empty, committed lines final, a short live region of steady height. It had to work in Windows Terminal and over SSH, with a fallback for 16 colours, for `NO_COLOR` and for ASCII on the old console host. It had to use keys that work everywhere, and its accent's contrast had to be measured on dark and light backgrounds.

**How the proposals were made.** One script held every screen as rows of styled spans. It drew the same scenario (a lexer fix, with a failing test and a streaming answer) in every proposal, at 100 and 60 columns, on Windows Terminal's dark Campbell scheme and on One Half Light. It wrote a comparison page and an ANSI file for each screen. A small program drew every screen with `cortex_term::Inline` into the test kit's virtual terminal, compared each cell with the design, and measured each streaming frame. Every screen of every round drew without a difference.

## The rounds

| Round | Proposals | The owner's verdict |
|---|---|---|
| 1 | **Flow** (09-tui's draft made concrete), **Outline** (numbered prompts as headings, the status set into a rule), **Ledger** (a log with a label column and states as words), **Dock** (colour-forward: a solid status bar, the rule as the context gauge, tool rounds on one line, panels as tabs) | Set aside: cluttered and crowded with text. The owner named Codex CLI's terminal UI as inspiration for the feel (air, little chrome, a quiet hierarchy), not as something to copy. |
| 2 | **Haze** (the prompt and the input on a faint purple band, one quiet status line), **Margin** (no backgrounds; prompts in the accent's tint, the rule above the input as the context gauge), **Tide** (the least on screen: a round as one sentence, a summary line per turn) | Set aside: still unconventional. The brief had banned the common idioms of terminal agents, and the owner asked for a conventional, modern UI. The brief was loosened: a glyph leading each block, a bounded input, a header card, tool calls as short bold lines and a checklist plan are shared vocabulary. The glyphs, colours and wording stay ours. |
| 3 | **Roomy** (blank lines between blocks, a rounded input box with a hint row under it) and **Trim** (blank lines only between turns, the input between two rules) | **Roomy chosen**, with two changes: answers must stand apart from reasoning and tool calls, and tool calls go in boxes that expand, as in other terminal agents. |
| 3, revised | **Roomy 2**: Roomy with those two changes | Chosen. |

## Decision

The look is Roomy 2, as [architecture/09-tui.md](../architecture/09-tui.md) now specifies. Its reference screens are in [architecture/tui-look/](../architecture/tui-look/README.md).

- **Blocks are led by our glyphs:** `◆` in the accent for the user and for the input, `◇` for the answer, `∴` for reasoning, `·` for a notice, and a state mark for a tool call. Text sits two columns in.
- **The answer stands apart.** It is the only text at full brightness outside a box. Its `◇` is in the terminal's own foreground: not dim, and not the accent, which keeps meaning "you". Reasoning and tool detail are dim.
- **One box per tool round,** with a dim rounded border: one line per call (the state mark, a bold imperative such as `Edit lex.rs`, the measure), and each call's detail under it when the box is open. A box per call would add two border rows to every call.
  - **Starts open:** a failure's last lines, and a diff of up to `tui.tool_detail` lines (12).
  - **Starts closed:** a longer diff, a successful command's output, and a read.
  - **While the round runs,** `Ctrl+T` opens or closes its box in place, and `Ctrl+P` focus reaches live boxes.
  - **Once committed,** a box stays as it was, and a closed one says `ctrl+o expands`.
- **The `Ctrl+O` viewer** shows the session on the alternate screen with every box closed. The arrow keys and `Enter` open one, and `Esc` returns.
- **Diffs** have a line-number and sign gutter beside the code, with added and removed lines tinted to the box's edge.
- **The input** is a rounded box, and **one hint row** replaces the status line. It shows the keys that matter now, and on its right the model always, with cost, context, speed and access only when they matter.
- **Prose wraps at 80 columns** (`tui.prose_width`). Code lines wider than the room are cut with `…`, not wrapped, so that a line stays a row.
- **The mouse is not used inline.** It is used in the viewer and the full-screen presenter only; the reasons are below.

## Why

- **It is what the owner asked for:** clean, modern and conventional, with room between blocks, where a newcomer finds the input at once.
- **It stays inside the presenter's limits.** While an answer streams, the live region is 6 rows. A token rewrites one row, about 140 bytes, and committing a line rewrites the region, about 1.1 KB, mostly the input box's borders, which change on no other frame. A running round adds its box: 10 rows closed and 18 open in the reference scenario.
- **It keeps the vision's interface policy:** a chat flow, panels that come and go, and the logo's purple used sparingly.

## The mouse, and why not inline

A terminal reports the mouse for the whole window or not at all.
- With reporting on, the wheel no longer scrolls the scrollback and a drag no longer selects. Those are the two things inline mode is for.
- Reporting cannot be turned on only over the live region.
- tmux passes clicks only in its own mouse mode.
- The inline presenter does not know which screen row its live region starts on, and finding out would take the cursor-position query that [decision 0002](0002-tui-rendering.md) avoids.

So the keys do the work inline. Clicks come with the screens that own the whole window, the viewer and the full-screen presenter. That costs an SGR mouse decoder in `cortex-term` (modes 1000 and 1006, about 150 lines with tests), and the Windows console's mouse input turned on while the alternate screen is shown. Step 25 builds both.

## Found on the way

These hold for any look, and 09-tui now says each:

1. **The theme must name its 16-colour accent.** `Color::for_depth` brings `#7A3CFB` to bright blue (12) in 16 colours, not magenta. The 256-colour reduction gives index 99, as expected.
2. **Plain blue is unreadable on dark.** Windows Terminal's default blue is 2.4 to 1 on its dark background. Blue on a dark background means bright blue (5.0 to 1).
3. **On a dark background at 16 colours, no magenta reaches 4.5 to 1** (2.4 and 3.2 on Campbell). There the accent marks and does not carry text.
4. **`⏵`, the draft's chip glyph, is in neither Cascadia Mono nor Consolas.** The chosen look does not use it. Its glyphs are to be checked by eye in Windows Terminal with the reference screens.
5. **Over-wide code lines are cut, not wrapped**, as above. Inline code is bold as well as cyan on a light background, where cyan is 3.3 to 1.

## What is given up

- **The draft's ideas that were not chosen:** numbered turns as addresses (Outline), the model labelling each answer (Ledger), and the context gauge drawn as a rule (Dock, Margin).
- **Cost and the cache rate are not always on screen.** They show after a turn, when a budget is set, or in `/usage`. The vision's "always visible" is now "a glance away", by the owner's choice in round 2. [vision.md](../vision.md) says so.
- **Earlier detail waits for its round.** Details of calls that finish early in a round commit only when the round ends.
- **Six live rows while streaming,** where the draft had four.

## Revisit when

- Real use shows the live region's height or the box borders costing more than the budgets allow on a slow terminal.
- Windows Terminal's default font lacks one of the glyphs: the ASCII form or another glyph replaces it.
