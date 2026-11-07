pub fn pricing(provider: &str, model: &str) -> Option<(f64, f64)> {
    let m = model.to_ascii_lowercase();
    let key = format!("{}/{}", provider, m);
    let table: &[(&str, f64, f64)] = &[
        ("openai/gpt-4o", 2.50, 10.00),
        ("openai/gpt-4o-mini", 0.15, 0.60),
        ("openai/gpt-4.1", 2.00, 8.00),
        ("openai/gpt-4.1-mini", 0.40, 1.60),
        ("openai/gpt-4.1-nano", 0.10, 0.40),
        ("openai/o3-mini", 1.10, 4.40),
        ("openai/gpt-5-mini", 0.25, 2.00),
        ("anthropic/claude-3-5-haiku-latest", 0.80, 4.00),
        ("anthropic/claude-3-7-sonnet-latest", 3.00, 15.00),
        ("anthropic/claude-sonnet-4", 3.00, 15.00),
        ("anthropic/claude-opus-4", 15.00, 75.00),
        ("groq/llama-3.3-70b-versatile", 0.59, 0.79),
        ("groq/llama-3.1-8b-instant", 0.05, 0.08),
        ("groq/mixtral-8x7b-32768", 0.24, 0.24),
        ("openrouter/openai/gpt-4o-mini", 0.15, 0.60),
    ];
    table
        .iter()
        .find(|(k, _, _)| *k == key)
        .map(|(_, pin, pout)| (*pin, *pout))
}

pub fn estimate_prompt_tokens(prompt: &str) -> f64 {
    let words = prompt.split_whitespace().count() as f64;
    let chars = prompt.chars().count() as f64;
    let by_words = words * 1.33;
    let by_chars = chars / 4.0;
    by_words.max(by_chars)
}

pub fn format_usd(v: f64) -> String {
    if v == 0.0 {
        "0".to_string()
    } else if v < 0.01 {
        format!("${:.6}", v)
    } else {
        format!("${:.3}", v)
    }
}
