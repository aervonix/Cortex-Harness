| Item | Who | When |
|---|---|---|
| **An A/B run ends at a failing test.** `cargo xtask ab` stops at the first round in which a test fails, on either side, after both trees were built: a test that fails some of the time loses the rounds measured before it, and a change that leaves a test failing cannot be measured at all. Found by the independent review of the third tooling step. To settle: record the round as failed, leave that side's failing tests out of the medians, carry on, and say so in the report. | A later tooling step | When an A/B run is lost to it |
