# The protocol, generated

The two files here describe the Cortex-Harness protocol for programs that are not written in Rust. They are **generated** from the types in `crates/cortex-protocol` and are not edited by hand.

| File | What it is |
|---|---|
| [schema/cortex-protocol.schema.json](schema/cortex-protocol.schema.json) | One JSON Schema document (draft 2020-12). Every type is under `$defs`. The root accepts any JSON-RPC message. |
| [ts/cortex-protocol.d.ts](ts/cortex-protocol.d.ts) | TypeScript declarations for every type. Translated from the schema, so the two cannot disagree. |

The protocol itself, with its transports, versioning rules and conventions, is specified in [../architecture/11-rpc-protocol.md](../architecture/11-rpc-protocol.md).

## Finding the type of a message

A line on the wire is a JSON-RPC request, response or notification. What its `params` or `result` holds depends on the method.

- **In the schema,** the table `x-cortex-methods` has three parts: `client` (requests a client sends), `server` (requests the server sends) and `notifications`. Each entry gives the schema of the method's `params`, and of its `result` where it has one. `x-cortex-protocol-version` is the protocol version the document describes.
- **In the declarations,** `ClientMethods`, `ServerMethods` and `ServerNotifications` are the same table as types: `ClientMethods["turn/start"]["params"]` is `TurnStartParams`.

Every event arrives as the `params` of a `session/event` notification, and is a `SessionEvent`.

## Open enumerations and unions

Every enumeration and every tagged union in the protocol is open: a later version may add values, and a receiver must tolerate the ones it does not know.

- **In the schema,** the last branch of such a type is marked `"x-cortex-open": true`. For a string enumeration it is "any other string". For a union it is "an object whose `type` or `kind` is none of the known ones".
- **In the declarations,** a string enumeration ends with `(string & {})`, which accepts any string while editors still offer the known values. A union lists only the variants this version knows, so that narrowing on `type` or `kind` works; a client must still have a default branch for a variant that is not listed.

## Enforced values

Leases, grants, rules, sandbox policies, budgets, hard limits and stop conditions are the exception to "tolerate what you do not know": a reader refuses one that has a field or a value it does not know, whole ([decision 0028](../decisions/0028-enforced-values-fail-closed.md)).

- **In the schema,** these types have `"additionalProperties": false`, and their enumerations have no open branch.
- **In the declarations,** their enumerations are plain unions of literals, without `(string & {})`.

A client that reads a lease must do the same, and must not strip fields it does not know before passing a lease on.

## Changing the protocol

1. Change the types in `crates/cortex-protocol`.
2. Run `cargo xtask schema`.
3. Commit the types and the regenerated files together. The diff of these files is the protocol change, and belongs in the review.

`cargo xtask check` fails when the files are stale, and so does a test that runs on Linux and on Windows. Whether a change needs a new protocol version is decided by the rules in [../architecture/11-rpc-protocol.md](../architecture/11-rpc-protocol.md), "Handshake and versioning".
