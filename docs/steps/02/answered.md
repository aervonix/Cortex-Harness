# Answered in step 2, on 2026-11-13

Step 2 built CI. This record is of a later addition to it, made after wave 5.

## The owner guard, 2026-11-13

| Question | The owner's answer, 2026-11-13 | Recorded in |
|---|---|---|
| **How rule 3 is checked.** The owner's name was found once in a test comment on the main branch (fixed in pull request 32). No repository rule could look for it, since the rule would have to hold the name, and the repository is public. | **A blocking step in the `ci` workflow, on Linux, fed from a secret only the owner holds,** `OWNER_TERMS`: it fails a pull request, or a push to the main branch, whose added lines, paths or commits (messages, authors, committers) contain a term. It never prints a term. Without the secret it passes with a warning. | [testing-and-ci.md](../../testing-and-ci.md), "The owner guard"; rule 3 in [build-plan.md](../../build-plan.md) and `AGENTS.md` |
| **Where it runs on a push to the main branch.** The request named the `main` workflow. | **In the `ci` workflow's `lint-linux` job,** which runs on every push to the main branch as well as on pull requests: one step for both, and blocking, where the `main` workflow blocks nothing and would build `xtask` a second time. | `.github/workflows/ci.yml` |
