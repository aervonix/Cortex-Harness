# vllm-reasoning

Reasoning and then an answer from vLLM, which puts reasoning in `delta.reasoning`. Its documentation says the member was called `reasoning_content` in earlier versions, and that a client still reading the old name gets nothing.

- **Written by hand** on 2026-11-13, from vLLM's documentation of reasoning outputs (docs.vllm.ai/en/latest/features/reasoning_outputs.html). It was not recorded: there was no server of this kind to record from. A recording from the real server can replace it.
- Model: `Qwen/Qwen3-8B`. Speed asked for: `standard`.
- `request.json` is what the request builder writes for the request such a response would answer. It was not sent to anything.
