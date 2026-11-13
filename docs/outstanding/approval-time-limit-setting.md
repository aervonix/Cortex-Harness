| Item | Who | When |
|---|---|---|
| **A setting for how long an approval may wait.** The core takes the limit as an option (`CoreOptions::approval_timeout`), none by default: a person at a terminal is not hurried. The protocol says an unanswered request is a denial after "the configured time" ([architecture/11-rpc-protocol.md](../architecture/11-rpc-protocol.md)); the key belongs with the RPC server's client timeouts, in the reserved `[rpc]` section. | Step 27 | In that step |
