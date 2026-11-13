# Still outstanding

What the owner or a later step still has to settle: one file per item, each a one-row table of the item, who settles it, and when. Together they are the list that "Still outstanding" in [build-plan.md](../build-plan.md) points to.

- **Leaving something open:** add a file, named for the item in lowercase words joined by hyphens, with the table's header row `| Item | Who | When |`.
- **Settling an item:** delete its file, and record the answer in the settling step's `steps/<step>/answered.md`.
- **Changing an item:** edit its file. An item that several steps share stays one file.

One file per item, and not one per step, because a step settles items that other steps left open: two steps that settle different items then touch different files.
