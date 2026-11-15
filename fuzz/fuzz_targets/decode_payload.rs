#![no_main]

use libfuzzer_sys::fuzz_target;
use llmrace::provider::{decode_payload, Wire};

fuzz_target!(|data: &[u8]| {
    // decode_payload parses untrusted provider JSON across every wire —
    // it must never panic regardless of input.
    if let Ok(text) = std::str::from_utf8(data) {
        for wire in [
            Wire::OpenAiCompat,
            Wire::Anthropic,
            Wire::Gemini,
            Wire::Cohere,
        ] {
            let _ = decode_payload(text, wire);
        }
    }
});
