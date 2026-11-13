| Item | Who | When |
|---|---|---|
| **`config/reload` and the reload of a running session's configuration.** `cortex-config` has the live configuration with its "old configuration stays on error" rule; the core loads configuration once for each session and answers `config/reload` with `not_available`. What a reload applies to a running session, key by key (the engine's rules, the retry policy, the log's level), is to be wired. | Step 23 (the interactive application, where `/reload` is) | In that step |
