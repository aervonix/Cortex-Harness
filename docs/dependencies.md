# Dependencies

Every direct third-party dependency of the workspace: which crate uses it, why it is here, what else was considered, and what it costs. The policy it follows is in [architecture/01-workspace.md](architecture/01-workspace.md) ("Third-party dependency policy"), and the measurements behind the choices are in [measurements/dependency-costs.md](measurements/dependency-costs.md).

**No new dependency without a row here.** `cargo xtask rules` compares the table below with the manifests and fails when they disagree: a dependency with no row, a row with no dependency, or a row that names the wrong crates. It is part of the blocking checks.

## Adding a dependency

1. Check that it passes the policy: it earns its place over the standard library, it needs no build tool beyond the platform's compiler, and its licence is on the allow-list in [testing-and-ci.md](testing-and-ci.md).
2. Add it to `[workspace.dependencies]` in the root `Cargo.toml` with `default-features = false`, and opt in to the features that are used.
3. Reference it from the one crate that owns it with `name.workspace = true`.
4. Add a row to the table below. Measure the cost the way [measurements/dependency-costs.md](measurements/dependency-costs.md) describes, or cite the figure from that page.
5. Run `cargo xtask check`. The lockfile changes with the manifest and is committed with it.

## Direct dependencies

The first cell is the crate's name on crates.io and the second the workspace crates that depend on it. Both are checked. The table is in alphabetical order.

<!-- dependency-table:start -->
| Dependency | Used by | Kind | Why it is here | Considered instead | Cost |
|---|---|---|---|---|---|
| `serde_json` | `xtask` | normal | Reads the output of `cargo metadata` for the crate dependency rules and the check of this table. | Parsing the manifests by hand: it would miss what Cargo resolves (inherited keys, renames, default features), and the rules are defined on Cargo's view. | Not shipped: `xtask` is developer tooling. Used without `serde_derive`, so it adds 5 crates and no procedural macro. Dev build of the 5 crates: about 1 s on the desktop. It is in the planned set for the shipped crates in any case ([measurements/dependency-costs.md](measurements/dependency-costs.md)). |
<!-- dependency-table:end -->

## Tools

These are not Cargo dependencies and are not checked against the table. They are what the toolchain file and the CI workflow pin.

| Tool | Version | Pinned in | Used for |
|---|---|---|---|
| Rust (rustc, cargo, clippy, rustfmt) | 1.99.1 | `rust-toolchain.toml` | Everything. The project follows current stable and promises no minimum supported version. |
| cargo-nextest | 0.9.151 | `.github/workflows/ci.yml` | The test runner. Installed as a prebuilt binary. |
| cargo-deny | 0.20.2 | `.github/workflows/ci.yml` | Licence, ban and source checks. Installed as a prebuilt binary. |

GitHub Actions used by the blocking workflow, each pinned by commit hash in the workflow file:

| Action | Version | Used for |
|---|---|---|
| `actions/checkout` | v7.0.1 | Checking out the repository. |
| `Swatinem/rust-cache` | v2.9.3 | Caching the registry and the built dependencies. |
| `taiki-e/install-action` | v2.90.3 | Installing cargo-nextest and cargo-deny as checksummed prebuilt binaries. |
