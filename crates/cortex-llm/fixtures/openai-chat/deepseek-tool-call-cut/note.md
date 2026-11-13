# deepseek-tool-call-cut

A response cut off by the output limit while it was writing tool calls: the stop reason `length`, and a call whose arguments are not finished.

- Recorded on 2026-11-13 from the real API, with `cargo xtask record --provider deepseek`.
- Model: `deepseek-flash`. Speed asked for: `standard`.
- Identifiers and opaque payloads are stand-ins, and only the response headers that describe the response are kept (`docs/testing-and-ci.md`, "Fixtures").
