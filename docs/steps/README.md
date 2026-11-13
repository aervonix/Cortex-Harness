# Step records

What each build step recorded about itself: one folder per step, so that two steps that are written at the same time never edit the same lines.

| File | What it holds | It continues |
|---|---|---|
| `answered.md` | The questions the step put to the owner, and the owner's answers ("Answered in step N") | [build-plan.md](../build-plan.md), "The owner's decisions" |
| `ci.md` | The step's CI times, and what its first runs showed ("After step N") | [testing-and-ci.md](../testing-and-ci.md), "Where the five minutes go" |

- **A folder is named for its step,** with the number in two digits and the step's letter if it has one: `04`, `12`, `13a`. The folders then sort in step order, and reading them in turn reads the records in step order.
- **Work that is not a numbered step** has a folder named for it in lower-case words joined by hyphens, such as `tooling-w5`, with the same two files. Its headings have the work's name in place of `step N`: `# Answered in the wave 5 tooling step, on <date>`.
- **`cargo xtask rules` checks the layout:** the folder names, the two file names, and the first line of each. `cargo xtask links` runs that check, and the check of links, by themselves.
- **A step writes only in its own folder,** and only the files it has something for. The heading of each file is the heading its section had on the shared page, as a top-level heading: `# Answered in step N, on <date>` and `# After step N: <what it built>`.
- **Links are relative to the file:** `../../architecture/…` for an architecture page, `../12/answered.md` for another step's record.
- **What stays on the shared pages:** the general text, the owner's decisions made before step 4, and step 2's measurements, which the later records compare against.

Items still open are not step records. Each is a file in [outstanding/](../outstanding/README.md).
