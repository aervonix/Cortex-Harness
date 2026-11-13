# error-speed-not-offered

What the provider answers when a request names a speed, even `standard`, for a model that has no fast mode: a 400. It is why the builder leaves `speed` out for a model whose catalog entry offers no tier. The request was made with a catalog entry altered for the purpose; the built-in catalog would not have sent it.

- Recorded on 2026-11-13 from the real API, with `cargo xtask record speed-not-offered`.
- Model: `claude-haiku-5-5`. Speed asked for: `standard`.
- Identifiers and opaque payloads are stand-ins, and only the response headers that describe the response are kept (`docs/testing-and-ci.md`, "Fixtures").
