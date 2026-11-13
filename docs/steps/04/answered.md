# Answered in step 4, on 2026-11-12

Step 4 built `cortex-base`. These are the choices it put to the owner, each accepted as recommended.

| Question | The owner's answer, 2026-11-12 | Recorded in |
|---|---|---|
| **A dependency outside the planned set:** `unicode-segmentation`, for grapheme clusters. Width is counted per cluster, and a cut or a wrap must never split one. | **Approved**, with its measured cost in the dependency table. | [dependencies.md](../../dependencies.md), [architecture/01-workspace.md](../../architecture/01-workspace.md) |
| **Property tests without a property-testing crate:** a small helper on the seeded generator of `cortex-base`. | **Approved**, on two conditions, both met: a failure prints the seed and the input, and an environment variable multiplies the number of cases so that the nightly run checks many more than a pull request. Steps 6, 10 and 12 follow the pattern. | [testing-and-ci.md](../../testing-and-ci.md), "Property tests" |
| **Paths that could mean two things are refused, not tidied.** On Windows: device names, the device and extended-length forms, names that end with a dot or a space, alternate data streams, drive-relative and root-relative paths, and network shares that are not allowed in configuration. On every OS: a path that is not valid Unicode, and a link to something that does not exist. | **Approved.** The Windows refusals apply on Windows only. A path that is not Unicode is a refusal of that one path, which a directory walk steps past. Junctions are resolved like symbolic links, and a short name resolves to the long one. | [architecture/05-tools.md](../../architecture/05-tools.md), "Paths" |
| **An atomic write refuses a read-only target,** on both OSes. | **Approved.** The review of the step widened it, in the same spirit: a file the user may not write and anything that is not a regular file are refused too. | [architecture/05-tools.md](../../architecture/05-tools.md), "Atomic writes" |
| **The fake clock and the seeded generator live in `cortex-base`,** beside their traits, and the test kit re-exports them. | **Approved.** | [architecture/01-workspace.md](../../architecture/01-workspace.md), rule 6 |
