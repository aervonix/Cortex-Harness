# llamacpp-reasoning

Reasoning and then an answer from the llama.cpp server, which puts reasoning in `delta.reasoning_content` (its `--reasoning-format deepseek`). The last chunk carries the usage, with the tokens it reused from its prompt cache, and the server's own `timings`, which the decoder has no use for and skips.

- **Written by hand** on 2026-11-13, from the llama.cpp server's README (github.com/ggml-org/llama.cpp, tools/server/README.md). It was not recorded: there was no server of this kind to record from. A recording from the real server can replace it.
- Model: `qwen3-8b-q4_k_m.gguf`. Speed asked for: `standard`.
- `request.json` is what the request builder writes for the request such a response would answer. It was not sent to anything.
