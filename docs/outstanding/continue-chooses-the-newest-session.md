| Item | Who | When |
|---|---|---|
| **Which session `--continue` continues.** [10-headless.md](../architecture/10-headless.md) says "the latest session here". The core's `session/list` orders by session id, which is by creation, so after `--resume` of an older session, `--continue` takes the newest created, not the one used last. Found by the independent review of step 13b. Ordering by `updated_at` is a change to `cortex-core`'s list. | Step 23 (the session picker), which has the same choice to make | Before the first release |
