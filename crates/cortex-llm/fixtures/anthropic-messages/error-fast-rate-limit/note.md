# error-fast-rate-limit

A request at fast speed refused by the fast tier's own rate limit: a 429 with `retry-after`, and the tier's headers showing no output tokens left.

- Written by hand on 2026-11-13, from the provider's documentation of fast mode's rate limits, with the header names and limits a recorded fast response carries. It was not recorded: the case cannot be provoked on demand.
- Model: `claude-opus-5-5`. Speed asked for: `fast`.
- `request.json` is the request of a recorded fixture, standing in for the request that such a response would answer.
