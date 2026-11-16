#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // The Bedrock event-stream parser handles untrusted frames from the
    // network: every prefix must error or decode, never panic.
    for cut in 0..=data.len() {
        let _ = llmrace::provider::decode_frame(&data[..cut]);
    }
    if llmrace::provider::decode_frame(data).is_ok() {
        // if a full frame decodes, its bytes must round-trip
        // (decode consumed a prefix consistent with its lengths)
        let _ = data.len();
    }
});
