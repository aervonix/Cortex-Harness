//! Fuzzes the parser of server-sent events: any bytes, cut anywhere.
#![no_main]
#![allow(
    clippy::disallowed_types,
    reason = "the fuzzer's own macro reads a crash file; nothing here is the product"
)]

libfuzzer_sys::fuzz_target!(|data: &[u8]| cortex_fuzz::sse_parser(data));
