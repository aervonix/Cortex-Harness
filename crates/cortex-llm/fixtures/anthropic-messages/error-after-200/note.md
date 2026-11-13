# error-after-200

An error after a successful status: the response starts, and then the provider reports that it is overloaded. In a response that is not streamed this would be a 529.

- Written by hand on 2026-11-13, from the provider's documented error event. It was not recorded: the case cannot be provoked on demand.
- Model: `claude-haiku-5-5`. Speed asked for: `standard`.
- `request.json` is the request of a recorded fixture, standing in for the request that such a response would answer.
