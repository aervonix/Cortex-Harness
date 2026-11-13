# lmstudio-think-tags

Reasoning written inside the text, between `<think>` and `</think>`: what LM Studio sends for a reasoning model when its setting to separate reasoning from content is off. The tags are cut across chunks, as a model's tokens cut them.

- **Written by hand** on 2026-11-13, from LM Studio's documentation of its OpenAI-compatible endpoints and its API changelog (lmstudio.ai/docs/developer), which describe the setting that moves reasoning out of the content. It was not recorded: there was no server of this kind to record from. A recording from the real server can replace it.
- Model: `qwen/qwen3-8b`. Speed asked for: `standard`.
- `request.json` is what the request builder writes for the request such a response would answer. It was not sent to anything.
