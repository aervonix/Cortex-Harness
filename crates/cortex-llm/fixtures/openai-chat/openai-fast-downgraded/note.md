# openai-fast-downgraded

A request at fast speed that the provider served at standard speed: every chunk says `service_tier: "default"`. The fast-mode guide: "If your traffic ramps too fast, the system may downgrade some Fast mode requests to standard speeds and charge standard rates", and the response then shows `default`.

- **Written by hand** on 2026-11-13, from OpenAI's API reference for the chat completion chunk object and its guides to fast mode and streaming (developers.openai.com/api). It was not recorded: there was no OpenAI key, and a downgrade cannot be provoked on demand. A recording from the real server can replace it.
- Model: `gpt-6-luna`. Speed asked for: `fast`.
- `request.json` is what the request builder writes for the request such a response would answer. It was not sent to anything.
