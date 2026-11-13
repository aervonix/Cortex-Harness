# deepseek-insufficient-resource

A response the server cut short for lack of capacity: the stop reason `insufficient_system_resource`, which is not a stop reason at all but a failure in the middle of a response.

- **Written by hand** on 2026-11-13, from DeepSeek's API reference for the chat completion chunk (api-docs.deepseek.com/api/create-chat-completion). It was not recorded: the case cannot be provoked on demand. A recording from the real server can replace it.
- Model: `deepseek-flash`. Speed asked for: `standard`.
- `request.json` is what the request builder writes for the request such a response would answer. It was not sent to anything.
