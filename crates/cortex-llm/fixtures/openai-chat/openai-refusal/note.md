# openai-refusal

A refusal: its text arrives in `delta.refusal` and not in `content`, and the response stops with `stop`.

- **Written by hand** on 2026-11-13, from OpenAI's API reference for the chat completion chunk object and its guides to fast mode and streaming (developers.openai.com/api). It was not recorded: there was no OpenAI key. A recording from the real server can replace it.
- Model: `gpt-6-luna`. Speed asked for: `standard`.
- `request.json` is what the request builder writes for the request such a response would answer. It was not sent to anything.
