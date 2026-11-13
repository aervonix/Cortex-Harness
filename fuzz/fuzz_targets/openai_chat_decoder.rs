//! Fuzzes the decoder of the `openai-chat` protocol: any bytes as a
//! response stream, cut anywhere, for servers with each kind of flag.
#![no_main]
#![allow(
    clippy::disallowed_types,
    reason = "the fuzzer's own macro reads a crash file; nothing here is the product"
)]

libfuzzer_sys::fuzz_target!(|data: &[u8]| cortex_fuzz::openai_chat_decoder(data));
