#![no_main]

use libfuzzer_sys::fuzz_target;
use llmrace::sse::SseParser;

fuzz_target!(|data: &[u8]| {
    let mut parser = SseParser::new();
    let mut total_blocks = 0usize;

    let (first, second) = data.split_at(data.len() / 2);
    for chunk in [first, second] {
        match parser.feed(chunk) {
            Ok(blocks) => total_blocks += blocks.len(),
            Err(_) => return,
        }
    }
    total_blocks += parser.finish().len();

    assert!(total_blocks <= data.len() / 2 + 1);
});
