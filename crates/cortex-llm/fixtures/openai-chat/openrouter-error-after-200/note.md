# openrouter-error-after-200

A response that fails after it began: a chunk with an `error` at its top level, whose code is an HTTP status, and a choice that stops with `error`. OpenRouter sends the successful status as soon as an upstream provider accepts the request, so a later failure can only arrive this way.

- **Written by hand** on 2026-11-13, from OpenRouter's documentation of streaming and of errors (openrouter.ai/docs/api/reference/streaming, errors-and-debugging). It was not recorded: a provider that fails in the middle of a response cannot be provoked on demand. A recording from the real server can replace it.
- Model: `z-ai/glm-5.3-flash`. Speed asked for: `standard`.
- `request.json` is what the request builder writes for the request such a response would answer. It was not sent to anything.
