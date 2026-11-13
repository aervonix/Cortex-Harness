# Answered in step 6, on 2026-11-12

Step 6 built the tool contract and the read-only tools in `cortex-tools`. These are the choices it put to the owner.

| Question | The owner's answer, 2026-11-12 | Recorded in |
|---|---|---|
| **A content hash, and where it lives.** A file snapshot records one, the version of a file the model last saw is kept under one, and step 8 needs the same for checkpoints and the project key. Nothing in the planned set provided it. | **`cortex_base::ContentHash`, an agreed addition to `cortex-base`, through the `sha2` crate** and not SHA-256 of our own: `read` hashes whole files, the processor's SHA instructions make that several times faster, and a widely reviewed implementation is better than one we maintain. The algorithm is named nowhere outside the module. The standard's examples are kept as a test. | [dependencies.md](../../dependencies.md), [architecture/05-tools.md](../../architecture/05-tools.md), "File snapshots" |
| **How a cut result names its artifact.** The first draft said "the artifact path". A path would be an absolute path under the user's home directory, outside the workspace. A made-up form such as `artifact:<id>` was proposed. | **A separate argument.** `artifact:a_123` is a legal file name on Linux, and a path that could mean two things is refused, not guessed at. `read` and `search` take either `path` or `artifact`, never both, and a cut result names the artifact's id. | [architecture/05-tools.md](../../architecture/05-tools.md), "One output policy" |
| **`tokio` in `cortex-tools`,** with the `rt` feature only, for the blocking pool. | **Fine**, as the workspace page planned. Each crate names the `tokio` features it uses. | [dependencies.md](../../dependencies.md) |
