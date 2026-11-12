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
| `schemars` | `cortex-protocol` | normal, optional (feature `schema`) | Derives the JSON Schema of the protocol types, from the same serde attributes that define the wire form, so the schema cannot describe a different protocol from the one the code speaks. Used by `cargo xtask schema` and the schema test. No shipped crate turns the feature on, and `cargo xtask rules` fails if the dependency stops being optional. | Writing the schema by hand: it would drift from the types. Generating TypeScript directly with a second derive (ts-rs, typeshare): a second source to keep in step, where the declarations are now translated from the schema by about 250 lines in `xtask`. | Not shipped. Adds 6 crates to an `xtask` build (schemars, its derive, serde_derive_internals, ref-cast and its derive, dyn-clone) and about 2 s: a clean dev build of `cortex-protocol` with the feature took 9.2 to 9.3 s on 4 desktop cores, against 7.1 s without (step 3). Planned in [measurements/dependency-costs.md](measurements/dependency-costs.md) at 148 KB, which does not apply: it is never in the binary. |
| `serde` | `cortex-protocol` | normal | The protocol types are serde types: `Serialize` and `Deserialize` are derived for the structs, and written by hand for the open enumerations and unions ([architecture/11-rpc-protocol.md](architecture/11-rpc-protocol.md)). With the `derive` feature, which brings the one procedural macro every other crate will also use. | Hand-written JSON reading and writing for some 200 types: more code than the protocol itself, and every later crate needs serde in any case. miniserde and nanoserde: smaller, with no support for flattened or internally tagged data, which the event envelope needs, and the rest of the planned set (toml, reqwest, rmcp) depends on serde regardless. | In every shipped build. With `serde_json`: 11 crates and a clean dev build of `cortex-protocol` in 7.1 s on 4 desktop cores, of which the crate itself is 0.5 s and the rest is the derive macro's own dependencies (syn, quote, proc-macro2), built once for the whole workspace. Size: counted in the 259 KB the page measured for serde, serde_json and toml together ([measurements/dependency-costs.md](measurements/dependency-costs.md)). |
| `serde_json` | `cortex-protocol`, `xtask` | normal | In `cortex-protocol`: the JSON-RPC envelopes carry parameters and results as JSON values, tool arguments and structured tool payloads are JSON values, and the open unions read an object before they know its variant. In `xtask`: reads the output of `cargo metadata` for the crate dependency rules and the check of this table, and writes the generated schema. | For the protocol: a JSON value type of our own, which every crate that touches a tool call would then have to convert to and from. For `xtask`: parsing the manifests by hand, which would miss what Cargo resolves (inherited keys, renames, default features), and the rules are defined on Cargo's view. | In every shipped build: 4 crates beyond serde (itoa, memchr, zmij and itself). Its cost with serde is in the row above. |
<!-- dependency-table:end -->

## Tools

These are not Cargo dependencies and are not checked against the table. They are what the toolchain file and the CI workflow pin.

| Tool | Version | Pinned in | Used for |
|---|---|---|---|
| Rust (rustc, cargo, clippy, rustfmt) | 1.99.1 | `rust-toolchain.toml` | Everything. The project follows current stable and promises no minimum supported version. |
| cargo-nextest | 0.9.151 | `.github/workflows/ci.yml` | The test runner. Installed as a prebuilt binary. |
| cargo-deny | 0.20.2 | `.github/workflows/ci.yml`, `nightly.yml` | Licence, ban and source checks in the blocking workflow, and the advisory check nightly. Installed as a prebuilt binary. |
| cargo-machete | 0.9.2 | `.github/workflows/nightly.yml` | The nightly unused-dependency check (`cargo xtask unused`). It reads the sources, so it needs no nightly compiler and no build, and takes about a second. cargo-udeps was considered: it is exact where this is a heuristic, and it needs a nightly toolchain and a full build. Installed as a prebuilt binary. |

GitHub Actions used by the workflows, each pinned by commit hash in the workflow file:

| Action | Version | Used for |
|---|---|---|
| `actions/checkout` | v7.0.1 | Checking out the repository. |
| `Swatinem/rust-cache` | v2.9.3 | Caching the registry and the built dependencies. |
| `taiki-e/install-action` | v2.90.3 | Installing cargo-nextest, cargo-deny and cargo-machete as checksummed prebuilt binaries. |
| `actions/upload-artifact` | v7.0.2 | Keeping the size and crate-count record of each `main` and `nightly` run. Not used by the blocking workflow. |
