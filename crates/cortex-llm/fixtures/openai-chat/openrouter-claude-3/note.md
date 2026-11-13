# openrouter-claude-3

One request of a conversation with a model of Anthropic's through this server. The request carries cache markers, and the reasoning comes back in signed pieces (`reasoning_details`) that are sent back as they came.

- Recorded on 2026-11-13 from the real API, with `cargo xtask record --provider openrouter`.
- Model: `anthropic/claude-haiku-5.5`. Speed asked for: `standard`.
- Identifiers and opaque payloads are stand-ins, and only the response headers that describe the response are kept (`docs/testing-and-ci.md`, "Fixtures").
