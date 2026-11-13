# switch-2

One request of a conversation that changes model and changes back. The second model cannot read the first one's reasoning; every block is sent back unchanged all the same, and the provider leaves out what the model cannot read.

- Recorded on 2026-11-13 from the real API, with `cargo xtask record switch-2`.
- Model: `claude-haiku-5-5`. Speed asked for: `standard`.
- Identifiers and opaque payloads are stand-ins, and only the response headers that describe the response are kept (`docs/testing-and-ci.md`, "Fixtures").
