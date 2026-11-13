# venice-error-after-200

A response that begins with a successful status and then fails: the server accepted a tool whose schema is not one, started to answer, and sent an `error` in the stream.

- Recorded on 2026-11-13 from the real API, with `cargo xtask record --provider venice`.
- Model: `deepseek-v4-flash-0731`. Speed asked for: `standard`.
- Identifiers and opaque payloads are stand-ins, and only the response headers that describe the response are kept (`docs/testing-and-ci.md`, "Fixtures").
