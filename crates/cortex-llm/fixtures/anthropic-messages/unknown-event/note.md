# unknown-event

A plain text answer with things this build does not know mixed in: events of unknown types, one with data that is not JSON, two with no event name, an addition of an unknown kind to a text block, a comment line, and an event after the response has ended. Every one is skipped, and what is left decodes as the plain answer it is.

- Written by hand on 2026-11-13, from the provider's documentation of the stream, and the shape of the recorded fixtures beside this one. It was not recorded: the case cannot be provoked on demand.
- Model: `claude-haiku-5-5`. Speed asked for: `standard`.
- `request.json` is the request of a recorded fixture, standing in for the request that such a response would answer.
