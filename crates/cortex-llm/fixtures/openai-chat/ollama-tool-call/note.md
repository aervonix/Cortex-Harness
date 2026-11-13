# ollama-tool-call

A tool call from Ollama: the whole call in one delta, its arguments complete.

- **Written by hand** on 2026-11-13, from Ollama's documentation of its OpenAI compatibility (docs.ollama.com/api/openai-compatibility) and the response types of its source (`openai/openai.go`), which name the members. It was not recorded: there was no server of this kind to record from. A recording from the real server can replace it.
- Model: `qwen3:8b`. Speed asked for: `standard`.
- `request.json` is what the request builder writes for the request such a response would answer. It was not sent to anything.
