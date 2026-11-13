# deepseek-models-1

One request of a conversation that changes model inside this provider and changes back. The second model's entry does not say it reads the first one's reasoning, so the first model's response is sent to it without its reasoning, and with the member this server demands left empty.

- Recorded on 2026-11-13 from the real API, with `cargo xtask record --provider deepseek`.
- Model: `deepseek-flash`. Speed asked for: `standard`.
- Identifiers and opaque payloads are stand-ins, and only the response headers that describe the response are kept (`docs/testing-and-ci.md`, "Fixtures").
