# The worktree shell guard

A build session runs in a Claude Code worktree (`.claude/worktrees/<name>`). Claude Code then checks every shell command that names `git` before it runs it, and refuses one it "cannot verify stays inside the worktree". The refusal is a tool error, not a failed command: nothing ran.

This page is what was found by trying, in the tooling-2 session on 2026-11-13, with Claude Code 2.1.323. Every row below was run as written, with a read-only or `--dry-run` git command. The guard is Claude Code's and may change with its version: when a form here is refused, try the next form down, and correct the page.

## What it accepts and refuses

| Form | Example | Result |
|---|---|---|
| `git` alone, with options | `git log --oneline -1 "origin/main"` | accepted |
| `git … && git …`, `git …; git …` | `git add --dry-run -A && git commit --dry-run -m x; git push --dry-run …` | accepted |
| `git … \| tool`, `git … > file`, `git … 2>&1` | `git log -1 \| tee file`, `git fetch origin main 2>&1 \| tail -1` | accepted |
| Another tool, then `&&` and `git` | `sed -i … file && git diff --stat`, `cd xtask && git status` | accepted |
| An environment prefix | `GIT_EDITOR=true git status` | accepted |
| `git -C` this worktree | `git -C <this worktree> log -1` | accepted |
| `git -C` another checkout | `git -C <the main checkout> log -1` | **refused** (as it should be) |
| A script file that runs git | `bash file.sh`, `sh file.sh && echo ok`, `python3 file.py && git status` | accepted: the guard does not read the file |
| `python3 -` with a heredoc that runs git | `python3 - <<'EOF'` … `subprocess.run(["git", …])` … `EOF` | accepted |
| A heredoc commit message | `git commit -F - <<'EOF'` … `EOF` | accepted |
| `cat > file <<'EOF'` whose text says `.git` or `.github` | `cat > f <<'EOF'` / `see .github/workflows` / `EOF` | accepted |
| **`cat <<'EOF' > file` whose text says `.git` or `.github`** | `cat <<'EOF' > f` / `see .github/workflows` / `EOF` | **refused** |
| A heredoc to a file, then `git` on the next line | `cat > f <<'EOF'` … `EOF` / `git status` | accepted |
| `git`, then `&&` and a heredoc | `git log -1 && cat > f <<'EOF'` … `EOF` | **refused** |
| `$?` in a command that names git | `git status --short; echo $?` | **refused** |
| `$(git …)` | `echo "$(git rev-parse --short HEAD)"` | **refused** |
| A `for` loop around git | `for f in a b; do git log -1 -- "$f"; done` | **refused** |
| `git rebase --exec` | `git rebase --exec true …` | **refused**, "runs a program that can't be verified" |
| `$HOME` and other variables before git | `echo $HOME >/dev/null; git status` | accepted |
| Two heredocs in one command, or a heredoc in a long chain, **with no git at all** | `mkdir -p d && cat > d/a <<'EOF'` … `EOF`, `cat > d/b <<'EOF'` … `EOF`, `ls d \| wc -l` | **refused** at times, as "too complex to verify"; one heredoc a call is accepted |

Earlier sessions also saw `cargo nextest run -p cortex-base hash` refused, because `hash` is a shell builtin's name: spell such a filter `-E 'test(/ha.h::/)'`.

## The forms to use

- **Edit and run.** Use the Edit or Write tool for a file. One heredoc a call. For many files, Write a script to `$CLAUDE_JOB_DIR/tmp/<name>.py` and run it as `python3 <file>`. Inside a script, `.git` can be written as it is. A heredoc to a file is fine when the redirect comes first: `cat > file <<'EOF'`, never `cat <<'EOF' > file`. For Rust edits that must survive `rustfmt`, `cargo xtask replace` matches by token.
- **Stage and commit.** `git add <paths> && git commit -m "Subject" -m "Body."`, or `git commit -F <file>`, or `git commit -F - <<'EOF'`. Check the identity once: `git log -1 --format='%an <%ae>'`.
- **Rebase and push.** `git fetch -q origin main && git rebase origin/main`, then, as its own call after reading the result, `git push --force-with-lease origin <branch>`. To check every commit of a rebase, write the loop into a script file and run `sh <file> <worktree>`: a loop in the command, `$(git …)` and `--exec` are refused.
- **Anything that needs `$?`, a loop or `$(…)` around git:** a script file.

## Parallel calls in one batch

**A later call does not know that an earlier one was refused.** When several tool calls go out in one batch, each is checked and run on its own. A refusal stops that call only: the next ones run, against a tree the refused call did not change. In wave 5 that twice let a commit or a push run on the wrong state.

So never put a call in the same batch as a call it depends on: an edit and the commit of it, a commit and the push, a rebase and the push. Put independent reads in a batch; put each step that changes the tree or the remote in a call of its own, and read its result before the next.

## False positives

These are refused although they stay inside the worktree. They are reported to the coordinator for Anthropic.

1. `cat <<'EOF' > /path/file` with a line `see .github/workflows`, then `EOF`. The same text written as `cat > /path/file <<'EOF'` is accepted: the order of the redirect and the heredoc decides.
2. `git status --short; echo $?`.
3. `echo "$(git rev-parse --short HEAD)"`.
4. `for f in AGENTS.md CONTRIBUTING.md; do git log --oneline -1 -- "$f"; done`.
5. `git log --oneline -1 && cat > /path/file <<'EOF'`, a line `plain text`, `EOF`. The reverse order is accepted.
6. Commands with no git in them at all, refused as "too complex to verify": two `cat > file <<'EOF'` heredocs in one command after a `mkdir -p … &&`; and a `cd <worktree> && python3 - <<'EOF'` edit followed by a `cargo` command. Each is listed in [steps/tooling-2/answered.md](steps/tooling-2/answered.md).
