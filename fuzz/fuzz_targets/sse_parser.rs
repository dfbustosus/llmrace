#![no_main]

use libfuzzer_sys::fuzz_target;
use llmrace::sse::SseParser;

fn run_chunks<'a>(chunks: impl Iterator<Item = &'a [u8]>) -> Option<Vec<String>> {
    let mut parser = SseParser::new();
    let mut blocks = Vec::new();
    for chunk in chunks {
        match parser.feed(chunk) {
            Ok(mut out) => blocks.append(&mut out),
            Err(_) => return None,
        }
    }
    blocks.extend(parser.finish());
    Some(blocks)
}

fuzz_target!(|data: &[u8]| {
    let max_blocks = data.len() / 2 + 1;

    // 1. 2-way split at midpoint.
    let (first, second) = data.split_at(data.len() / 2);
    let two_way = run_chunks([first, second].into_iter());
    if let Some(ref blocks) = two_way {
        assert!(blocks.len() <= max_blocks);
    }

    // 2. 1-byte chunks (exercises split-CRLF and incremental-scan edges).
    let one_byte = run_chunks(data.chunks(1));
    if let Some(ref blocks) = one_byte {
        assert!(blocks.len() <= max_blocks);
        if let Some(ref expected) = two_way {
            assert_eq!(blocks, expected);
        }
    }

    // 3. Small variable chunks (1..=16 bytes) derived from input bytes.
    let mut cursor = 0usize;
    let mut step = data.first().copied().unwrap_or(0) as usize;
    let mut var_chunks = Vec::new();
    while cursor < data.len() {
        let len = (step & 0x0f) + 1;
        let end = (cursor + len).min(data.len());
        var_chunks.push(&data[cursor..end]);
        step = step.wrapping_mul(31).wrapping_add(data[cursor] as usize);
        cursor = end;
    }
    let variable = run_chunks(var_chunks.into_iter());
    if let Some(ref blocks) = variable {
        assert!(blocks.len() <= max_blocks);
        if let Some(ref expected) = two_way {
            assert_eq!(blocks, expected);
        }
        if let Some(ref expected) = one_byte {
            assert_eq!(blocks, expected);
        }
    }
});
