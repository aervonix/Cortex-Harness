# venice-error-fast-overloaded

A request at fast speed that the fast variant of the model refused for lack of capacity: a failed response with the code `MODEL_AT_CAPACITY`. The fast variant is a model of its own with capacity of its own, so the refusal is the tier's, and the request can be sent again to the standard model.

- **Written by hand** on 2026-11-13, from Venice.ai's documentation of its error codes (docs.venice.ai/api-reference/error-codes), which gives the status, the code and the message. It was not recorded: a model at capacity cannot be provoked on demand. A recording from the real server can replace it.
- Model: `deepseek-v4-flash-0731`. Speed asked for: `fast`.
- `request.json` is what the request builder writes for the request such a response would answer. It was not sent to anything.
