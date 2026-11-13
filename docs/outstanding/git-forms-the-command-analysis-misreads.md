| Item | Who | When |
|---|---|---|
| **Forms of git the command analysis misreads.** `git -c alias.x=config x core.fsmonitor ./run` runs `git config` through an alias, and is not seen as one, so it is not asked about where `git config` is. Outside a repository, `git diff a@x b@x` compares two files, but words that read as revisions keep it on the table of commands that only read. Found by the final independent review of the fixes to step 12. | Step 32 (hardening pass); the sandbox of step 26 holds the rest | In step 32 |
