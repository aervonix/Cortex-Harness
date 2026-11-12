# 0007: Configuration format

- **Status:** Accepted
- **Date:** 2026-11-12

## Context

Users and projects configure the harness in files that people write by hand, that live in repositories, and that are merged across several layers.

## Options considered

| Option | For | Against |
|---|---|---|
| TOML | Comments. Unambiguous types. Tables map directly onto named collections (`[providers.x]`, `[mcp.servers.y]`), which is what layered merging needs. The parser is one small pure-Rust crate that serde already supports. Familiar to anyone who has edited a `Cargo.toml`. | Deeply nested structures are awkward. Inline tables must fit on one line in older TOML versions. |
| JSON with comments | Widely known. Editors support JSON Schema directly. | Several dialects of "JSON with comments" exist. No standard. Trailing-comma and comment rules differ by tool. Writing nested objects by hand is error-prone. Both reference harnesses use JSON, and one of the rules of this project is not to copy their configuration formats. |
| YAML | Comments. Compact. | Type coercion surprises, indentation errors, a large specification, and no small well-maintained parser. |
| A scripting language | Maximum flexibility. | A configuration file that runs code is an attack surface and cannot be merged or validated statically. |

## Decision

TOML, one schema with a `config_version`, validated on load, with a generated JSON Schema for editor support.

## Why

- The configuration is mostly shallow tables of named things, which is TOML's strength.
- It needs comments, because people explain permission rules and overrides to their teammates in the file.
- Merging is defined on tables and scalars with no special list syntax ([architecture/08-configuration.md](../architecture/08-configuration.md)), which TOML's data model fits.
- It is not either reference harness's format.

## Revisit when

Configuration grows structures that TOML expresses badly, or users ask for a programmable configuration with a clear use that hooks and extensions do not cover.
