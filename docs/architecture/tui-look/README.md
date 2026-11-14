# The TUI's look: reference screens

These are the screens of the look the owner chose on 2026-11-14 ([decision 0033](../../decisions/0033-tui-look.md)), which [09-tui.md](../09-tui.md) specifies. They show one session: turn 3 in a Rust workspace, where the agent fixes digit separators in a lexer, runs the tests, sees one fail, and streams the fix while the user types a steering message.

- **Width.** Every screen is 100 columns, except screen 6 at 60. No row uses the last column.
- **The live region** is the last rows of each screen; the table says how many. In screens 8a and 8b the viewer owns the whole screen.
- **How they were checked.** The design round's script drew each screen through `cortex_term::Inline` into the test kit's virtual terminal and compared every cell. The script is not in the repository; these files are the record of what was chosen.

**To see a screen as the terminal draws it,** print its ANSI file in a terminal whose background matches the file's name:
- Linux: `cat docs/architecture/tui-look/1-dark.ans`
- PowerShell: `Get-Content -Raw docs\architecture\tui-look\1-dark.ans`

The `.txt` file beside each one is the same screen without colour, to read here.

| Screen | Shows | Rows, live | Files |
|---|---|---|---|
| 0 | The start of a session: the header card, the empty input box and the hint row | 11, 4 | [text](0.txt), [dark](0-dark.ans), [light](0-light.ans) |
| 1 | A turn in progress: your message, reasoning, an answer, a tool round with its diff and a failure, and the answer streaming a code block | 40, 6 | [text](1.txt), [dark](1-dark.ans), [light](1-light.ans) |
| 1b | While it reasons: the two live reasoning rows | 11, 7 | [text](1b.txt), [dark](1b-dark.ans), [light](1b-light.ans) |
| 1c | A round running, its box open: the default for a small diff | 29, 18 | [text](1c.txt), [dark](1c-dark.ans), [light](1c-light.ans) |
| 1d | The same round after `Ctrl+T`, closed | 21, 10 | [text](1d.txt), [dark](1d-dark.ans), [light](1d-light.ans) |
| 2a | An approval with a diff | 18, 12 | [text](2a.txt), [dark](2a-dark.ans), [light](2a-light.ans) |
| 2b | A request to leave the sandbox: run once or deny | 19, 11 | [text](2b.txt), [dark](2b-dark.ans), [light](2b-light.ans) |
| 3a | The `/model` menu | 11, 9 | [text](3a.txt), [dark](3a-dark.ans), [light](3a-light.ans) |
| 3b | The `/effort` menu, with `tab - fast mode` | 11, 9 | [text](3b.txt), [dark](3b-dark.ans), [light](3b-light.ans) |
| 4 | Panels: a running command and the plan, focused, with a limit warning | 43, 15 | [text](4.txt), [dark](4-dark.ans), [light](4-light.ans) |
| 5 | Notices (an instruction change, compaction with its token counts, a sandbox report, a speed fallback, a retry) and an error | 17, 4 | [text](5.txt), [dark](5-dark.ans), [light](5-light.ans) |
| 6 | Screen 1 at 60 columns | 42, 6 | [text](6.txt), [dark](6-dark.ans), [light](6-light.ans) |
| 7a | Screen 1 in 16 colours | 40, 6 | [text](7a.txt), [dark](7a-dark.ans), [light](7a-light.ans) |
| 7b | Screen 1 with `NO_COLOR` | 40, 6 | [text](7b.txt), [dark](7b-dark.ans), [light](7b-light.ans) |
| 7c | Screen 1 on the old console host: ASCII glyphs, 16 colours | 40, 6 | [text](7c.txt), [dark](7c-dark.ans), [light](7c-light.ans) |
| 8a | The `Ctrl+O` viewer, every box closed, the latest selected | 39, all | [text](8a.txt), [dark](8a-dark.ans), [light](8a-light.ans) |
| 8b | The viewer, the selected box opened with `Enter` | 51, all | [text](8b.txt), [dark](8b-dark.ans), [light](8b-light.ans) |

The colours in the dark files assume a dark terminal scheme and those in the light files a light one. The accent's tint for text differs between them, as 09-tui describes.
