| Item | Who | When |
|---|---|---|
| **Fuzz targets for the decoders still to come.** Step 14 added coverage-guided fuzzing: a `fuzz` job in the nightly workflow, with targets for the SSE parser and the Anthropic decoder ([testing-and-ci.md](../testing-and-ci.md), "Not blocking"). Each later protocol adds a target for its decoder to `fuzz/` and a line to the list in `xtask/src/outside.rs`. | Steps 31, 35 and 36 | In those steps |
