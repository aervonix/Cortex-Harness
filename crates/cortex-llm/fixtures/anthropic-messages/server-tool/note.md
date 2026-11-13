# server-tool

A server-side tool: the call the provider ran itself (`server_tool_use`), its result (`web_search_tool_result`), which arrives whole, and text with a citation added by a `citations_delta`. None of the three is a kind the neutral model names, so each is kept as a provider block, or as text with its citations in the stored block.

- Written by hand on 2026-11-13, from the provider's documented stream for the web search tool. It was not recorded: the case cannot be provoked on demand.
- Model: `claude-haiku-5-5`. Speed asked for: `standard`.
- `request.json` is the request of a recorded fixture, standing in for the request that such a response would answer.
