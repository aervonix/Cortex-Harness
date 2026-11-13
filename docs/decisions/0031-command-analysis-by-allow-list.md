# 0031: Shell commands are analysed by hand-written allow-list tokenizers

- **Status:** Accepted by the owner on 2026-11-13, in step 12.
- **Date:** 2026-11-13

## Context

A permission rule on a command (`allow exec cargo test *`), and the table of commands that only read, are useful only if a command cannot be made to look like another ([architecture/06-permissions-and-sandbox.md](../architecture/06-permissions-and-sandbox.md), "Shell command analysis"). The engine therefore has to say what a command line *is*: which commands, with which arguments. Where it cannot say, the answer must be "ask".

The page left the means to the permissions step: a pure-Rust parser crate, or a hand-written tokenizer. A C grammar was already excluded by the dependency policy. The same question stood for PowerShell, and for `cmd.exe`.

## Options considered

Measured on the desktop on 2026-11-13, 4 cores pinned, clean builds, by the method of [measurements/dependency-costs.md](../measurements/dependency-costs.md), with no other run active. Each candidate was also given 43 lines chosen to be awkward.

| Option | Grammar it covers | On input it does not understand | Upkeep | Dev build | Release build | Size | Packages |
|---|---|---|---|---|---|---|---|
| **A hand-written tokenizer that accepts a listed subset** | Simple commands, quotes, `\|`, `&&`, `\|\|`, `;`, plain redirections | Refuses: the character or the form is not on its list | Ours: about 700 lines for two shells | 0.2 s | 1.3 s | 27 KB | 0 |
| `brush-parser` 0.4.0 | bash, nearly all of it | Returns a tree. What is dangerous must be found in it, node by node | Active, MIT | 6.7 s | 10.9 s | 365 KB | 75 |
| `conch-parser` 0.1.1 | POSIX `sh`, not bash's additions | Parse errors for `<<<` and `\|&`; `&>` read as a background job | Last release 2019 | 0.5 s | 3.0 s | 226 KB | 3 |
| `yash-syntax` 0.25.0 | POSIX `sh` | | Active | not measured | | | GPL-3.0: not on the licence list |

What the 43 lines showed of `brush-parser`, the only serious parser candidate:

- `echo {a,b}` and `echo {1..3}`: the braces are plain text in the tree. Brace expansion is found by a third call, apart from the parser and the word parser.
- `ls &`, `! ls`, `time ls`: each is a plain `ls` command in the tree, with the difference in a flag on a parent node that a walker must remember to look at.
- `ls =cat`: plain text. zsh replaces `=cat` with the path of the program `cat`.
- A word is kept as raw text. Whether it holds an expansion is found by parsing each word again.

## Decision

**Command lines are analysed by tokenizers of our own that accept a small listed subset and refuse everything else.** They are in `cortex-policy` (`command::posix`, `command::powershell`).

- **POSIX shells** (bash, sh, dash, zsh, ksh): simple commands joined by `|`, `&&`, `||`, `;` and line breaks; words of letters, digits and `_ - . / : = + , @ %`; single quotes; double quotes with no `$` and no back-tick; `~` for the home directory; globs, kept and flagged; redirections `<`, `>`, `>>` to a plain word, and `2>&1`. Everything else is "not modelled": `$`, back-ticks, `( )`, `{ }` anywhere in a word, a backslash outside quotes, `&`, `|&`, here-documents, `<<<`, `>|`, `#`, `!`, `^`, a word that begins with `=`, `~user`, an assignment, a shell keyword, a builtin that runs text as code or changes what later commands mean, a quote left open.
- **PowerShell:** one command with literal arguments. No `;`, `|`, `&`, `$`, back-tick, bracket, `@`, `,`, `%`, `#`, `<`, `>`; a quote only around a whole word; no character outside ASCII outside quotes, and no typographic quote anywhere, because PowerShell reads several dashes as `-` and several quotes as `'` or `"`; no nested `powershell`, `pwsh`, `cmd` or `Invoke-Expression`, which is where an encoded command would be.
- **`cmd.exe` is not analysed.** Its quoting and its `%VAR%` and `^` rules depend on context in ways a tokenizer cannot follow safely.
- **A shell this build does not know is not analysed either** (`fish`, `nu`). The shell tool starts any unknown program as if it read `-c <command>`; that is no reason to believe it reads the command like a POSIX shell. `fish` reads a backslash inside single quotes as an escape.
- **Not analysed means:** no allow rule applies, no safe list applies, and the level decides the command like any other. A `deny` or `ask` rule is still matched, against the raw line, wherever its words appear: for a rule that holds back, matching too much is the safe side.

**The tokenizers are checked against the shells themselves.** A test generates lines, keeps the ones the analysis models, and compares: bash and dash must pass exactly the words the analysis gave, and PowerShell's own parser must read the line as one command with those words. On its first run that comparison found that dash reads `20<file` as the argument `20` and a redirection where bash reads descriptor 20, so several digits before a redirection are not modelled.

## Why

- **It fails closed by construction.** A parser accepts the language and leaves the refusing to its user. A tokenizer that lists what it accepts refuses what nobody thought of.
- **It is one subset for every shell we start.** A bash grammar would be wrong for dash and zsh in the corners that matter here.
- **It costs nothing to build or to ship,** against 75 packages and 365 KB for a parser whose words would still need the same check.
- **Rules on commands are a convenience layer, not a boundary** ([security.md](../security.md)). The right amount of grammar is the amount that covers what models write every day: `cargo test`, `git status && git diff`, `grep -rn x src | head`. A line outside it is a question to the user, which is what happened to every command before there were rules.

## What is given up

- **Lines a parser would have read are asked about:** `echo $HOME`, `find . -name \*.rs`, `for f in *.rs; do …; done`, `VAR=1 make`. Where that proves too often, the subset can grow one form at a time, each with its cases in the comparison against the real shells.
- **Upkeep is ours.** A new shell feature that changes what the subset means would be ours to notice. The comparison test runs against the shells the CI runners have, so a change in bash, dash or PowerShell that matters shows there.

## Revisit when

- Statistics from real sessions show a form outside the subset being asked about often enough to matter.
- A second POSIX-family shell with different quoting becomes a default somewhere we support.
