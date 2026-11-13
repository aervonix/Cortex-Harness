# tool-call-cut

A tool call cut off by the output limit in the middle of its arguments, as a tool with streamed arguments can be: the text of the arguments is not JSON. The provider does not close the block before it ends the response.

- Written by hand on 2026-11-13, from the provider's documentation of fine-grained tool streaming. It was not recorded: the case cannot be provoked on demand.
- Model: `claude-haiku-5-5`. Speed asked for: `standard`.
- `request.json` is the request of a recorded fixture, standing in for the request that such a response would answer.
