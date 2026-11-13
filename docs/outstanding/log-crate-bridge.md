| Item | Who | When |
|---|---|---|
| **The bridge for dependencies that log through the `log` crate.** [Decision 0019](../decisions/0019-logging.md) says their messages are bridged at `warn` and above. Step 13a built the subscriber without it: it needs `log` as a second direct dependency and a logger behind it, and nothing in the build today logs through `log` in a way a user would miss. | Step 32 (the hardening pass), or the first step whose dependency needs it | Before the first release |
