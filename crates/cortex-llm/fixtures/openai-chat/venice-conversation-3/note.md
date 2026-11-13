# venice-conversation-3

One request of a conversation with tools, on one model. Each request repeats the one before it and adds to it, and sends the model's own earlier responses back as they came, reasoning included.

- Recorded on 2026-11-13 from the real API, with `cargo xtask record --provider venice`.
- Model: `deepseek-v4-flash-0731`. Speed asked for: `standard`.
- Identifiers and opaque payloads are stand-ins, and only the response headers that describe the response are kept (`docs/testing-and-ci.md`, "Fixtures").
