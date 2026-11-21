//! Per-model pricing and prompt-token estimation for cost reporting.

use crate::error::{Error, Result};
use std::collections::HashMap;
use std::sync::OnceLock;

type Overrides = HashMap<String, TokenRates>;

fn overrides() -> &'static std::sync::RwLock<Overrides> {
    static OVERRIDES: OnceLock<std::sync::RwLock<Overrides>> = OnceLock::new();
    OVERRIDES.get_or_init(|| std::sync::RwLock::new(HashMap::new()))
}

/// USD per million tokens for one model.
///
/// A cache rate is `None` when this crate has no published price for that
/// bucket. [`usage_cost_usd`] then bills those tokens at [`Self::input`],
/// and the report says the discount was not applied.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TokenRates {
    /// Uncached input tokens.
    pub input: f64,
    /// Output tokens.
    pub output: f64,
    /// Cache-read tokens (a reused prompt prefix).
    pub cache_read: Option<f64>,
    /// Cache-write tokens at the short TTL. Anthropic's 5-minute write.
    pub cache_write: Option<f64>,
    /// Cache-write tokens at the long TTL. Anthropic's 1-hour write.
    pub cache_write_1h: Option<f64>,
}

/// Token counts for one priced round.
///
/// Cache counts are subsets of `prompt`. The uncached input is whatever
/// remains after the cache buckets are subtracted.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UsageTokens {
    /// Total input tokens, including cache reads and cache writes.
    pub prompt: f64,
    /// Tokens billed at the cache-read rate.
    pub cache_read: f64,
    /// Tokens billed at the short cache-write rate.
    pub cache_write: f64,
    /// Tokens billed at the 1-hour cache-write rate.
    pub cache_write_1h: f64,
    /// Completion tokens.
    pub output: f64,
}

/// A parsed `--price` value.
///
/// `provider/model=in:out` leaves the cache rates unset, so an override
/// does not silently keep a built-in cache discount. Optional further
/// segments are `cached`, `write` and `write_1h`, in that order.
#[derive(Debug, Clone, PartialEq)]
pub struct PriceOverride {
    /// Lowercased `provider/model` key.
    pub key: String,
    /// Rates from the argument. Unset cache segments are `None`.
    pub rates: TokenRates,
}

/// Installs user-supplied price overrides (`provider/model` → `(input,
/// output)` USD per million tokens). Cache rates are left unset: those
/// tokens are billed at the override's input rate. Values take precedence
/// over the built-in table.
pub fn set_overrides(pairs: Vec<(String, f64, f64)>) {
    set_rate_overrides(
        pairs
            .into_iter()
            .map(|(key, input, output)| {
                (
                    key,
                    TokenRates {
                        input,
                        output,
                        cache_read: None,
                        cache_write: None,
                        cache_write_1h: None,
                    },
                )
            })
            .collect(),
    );
}

/// Installs price overrides, including cache rates.
pub fn set_rate_overrides(pairs: Vec<(String, TokenRates)>) {
    let mut map = overrides().write().expect("price overrides lock");
    for (key, rates) in pairs {
        map.insert(key, rates);
    }
}

/// Parses a two-part `--price` argument: `provider/model=1.25:5`.
pub fn parse_override(raw: &str) -> Result<(String, f64, f64)> {
    let over = parse_price_override(raw)?;
    if over.rates.cache_read.is_some() {
        return Err(Error::Config(format!(
            "invalid --price '{raw}': parse_override only accepts in:out; use parse_price_override for cache rates"
        )));
    }
    Ok((over.key, over.rates.input, over.rates.output))
}

/// Parses `--price provider/model=in:out[:cached[:write[:write_1h]]]`.
pub fn parse_price_override(raw: &str) -> Result<PriceOverride> {
    let (key, prices) = raw.split_once('=').ok_or_else(|| {
        Error::Config(format!(
            "invalid --price '{raw}': expected provider/model=in:out[:cached[:write[:write_1h]]]"
        ))
    })?;
    let mut parts = prices.split(':');
    let input = required_rate(parts.next(), raw, "input")?;
    let output = required_rate(parts.next(), raw, "output")?;
    let cache_read = optional_rate(parts.next(), raw, "cached input")?;
    let cache_write = optional_rate(parts.next(), raw, "cache write")?;
    let cache_write_1h = optional_rate(parts.next(), raw, "1-hour cache write")?;
    if parts.next().is_some() {
        return Err(Error::Config(format!(
            "invalid --price '{raw}': at most five rates (in:out:cached:write:write_1h)"
        )));
    }
    if cache_write.is_some() && cache_read.is_none() {
        return Err(Error::Config(format!(
            "invalid --price '{raw}': a cache-write rate requires a cached-input rate"
        )));
    }
    if cache_write_1h.is_some() && cache_write.is_none() {
        return Err(Error::Config(format!(
            "invalid --price '{raw}': a 1-hour write rate requires a cache-write rate"
        )));
    }
    Ok(PriceOverride {
        key: key.trim().to_ascii_lowercase(),
        rates: TokenRates {
            input,
            output,
            cache_read,
            cache_write,
            cache_write_1h,
        },
    })
}

fn required_rate(raw: Option<&str>, ctx: &str, label: &str) -> Result<f64> {
    let Some(raw) = raw else {
        return Err(Error::Config(format!(
            "invalid --price '{ctx}': expected in:out prices"
        )));
    };
    parse_rate(raw, ctx, label)
}

fn optional_rate(raw: Option<&str>, ctx: &str, label: &str) -> Result<Option<f64>> {
    raw.map(|part| parse_rate(part, ctx, label)).transpose()
}

fn parse_rate(raw: &str, ctx: &str, label: &str) -> Result<f64> {
    let value: f64 = raw
        .trim()
        .parse()
        .map_err(|_| Error::Config(format!("invalid {label} price in '{ctx}'")))?;
    if !value.is_finite() || value < 0.0 {
        return Err(Error::Config(format!("invalid {label} price in '{ctx}'")));
    }
    Ok(value)
}

/// How a listed model bills a reused prompt prefix.
enum CacheSchedule {
    /// No published cache rate. Cache tokens are billed at the input rate.
    None,
    /// Cached input at this USD per million. No separate write price:
    /// OpenAI models through gpt-5, and Gemini context caching.
    Read(f64),
    /// Anthropic's published multipliers on the base input price:
    /// cache read 0.1×, 5-minute write 1.25×, 1-hour write 2×.
    /// The models in this table are the 0.1× read tier, not the 0.05× or
    /// 0.025× exceptions.
    Anthropic,
}

/// Day the OpenAI, Anthropic and Gemini rows were read from the pages
/// named in [`pricing_sources`]. Other providers' rows were not re-read
/// that day.
pub const PRICING_AS_OF: &str = "2026-11-20";

/// Pages the dated rows were read from. `(publisher, url)`.
#[must_use]
pub fn pricing_sources() -> &'static [(&'static str, &'static str)] {
    &[
        ("OpenAI", "https://developers.openai.com/api/docs/pricing"),
        (
            "Anthropic",
            "https://platform.claude.com/docs/en/about-claude/pricing",
        ),
        ("Gemini", "https://ai.google.dev/gemini-api/docs/pricing"),
    ]
}

/// One sentence a report can print under the cost column.
#[must_use]
pub fn pricing_stamp() -> String {
    let pages = pricing_sources()
        .iter()
        .map(|(name, url)| format!("{name} ({url})"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("OpenAI, Anthropic and Gemini rates were read on {PRICING_AS_OF} from {pages}.")
}

/// Returns `(input, output)` USD per million tokens, when known.
///
/// ```
/// use llmrace::cost::pricing;
///
/// let (input, output) = pricing("openai", "gpt-4o-mini").unwrap();
/// assert_eq!((input, output), (0.15, 0.60));
/// assert!(pricing("ollama", "llama3.2").is_none());
/// ```
#[must_use]
pub fn pricing(provider: &str, model: &str) -> Option<(f64, f64)> {
    token_rates(provider, model).map(|rates| (rates.input, rates.output))
}

/// Returns the published token rates for a provider/model pair.
///
/// Cache reads are the discounted input rate. Anthropic (and Claude on
/// Bedrock) also publishes a write premium. Hourly cache storage is not a
/// per-request token count, so it is not part of these rates. An override
/// replaces the whole row, including its cache rates.
///
/// ```
/// use llmrace::cost::token_rates;
///
/// let rates = token_rates("openai", "gpt-4o-mini").unwrap();
/// assert_eq!(rates.cache_read, Some(0.075));
/// assert!(rates.cache_write.is_none());
/// ```
#[must_use]
pub fn token_rates(provider: &str, model: &str) -> Option<TokenRates> {
    let key = format!("{}/{}", provider, model.to_ascii_lowercase());
    if let Ok(map) = overrides().read() {
        if let Some(rates) = map.get(&key) {
            return Some(*rates);
        }
    }
    // Input and output are the standard public list prices, USD per million
    // tokens, read 2026-11-20. OpenAI rates are the standard short-context
    // column on https://developers.openai.com/api/docs/pricing (cached input
    // is that page's cached column; gpt-4.1-nano has none). Anthropic rates
    // are https://platform.claude.com/docs/en/about-claude/pricing ; the
    // cache schedule is 0.1× read, 1.25× 5-minute write, 2× 1-hour write.
    // Gemini rates are the paid text tier on
    // https://ai.google.dev/gemini-api/docs/pricing . Hourly cache storage
    // is not a per-request token count and is not priced. Rows for other
    // providers were not re-read on that date. The Vertex lane copies the
    // Gemini Developer API cached rate for the same model. Not every model
    // id is listed: pass `--price` for one that is missing.
    let table: &[(&str, f64, f64, CacheSchedule)] = &[
        ("openai/gpt-4o", 2.50, 10.00, CacheSchedule::Read(1.25)),
        ("openai/gpt-4o-mini", 0.15, 0.60, CacheSchedule::Read(0.075)),
        ("openai/gpt-4.1", 2.00, 8.00, CacheSchedule::Read(0.50)),
        ("openai/gpt-4.1-mini", 0.40, 1.60, CacheSchedule::Read(0.10)),
        ("openai/gpt-5-mini", 0.25, 2.00, CacheSchedule::Read(0.025)),
        ("openai/gpt-5", 1.25, 10.00, CacheSchedule::Read(0.125)),
        ("openai/gpt-5-nano", 0.05, 0.40, CacheSchedule::Read(0.005)),
        ("openai/gpt-5.4", 2.50, 15.00, CacheSchedule::Read(0.25)),
        (
            "openai/gpt-5.4-mini",
            0.75,
            4.50,
            CacheSchedule::Read(0.075),
        ),
        ("openai/gpt-5.4-nano", 0.20, 1.25, CacheSchedule::Read(0.02)),
        ("openai/gpt-4.1-nano", 0.10, 0.40, CacheSchedule::None),
        (
            "anthropic/claude-3-5-haiku-latest",
            0.80,
            4.00,
            CacheSchedule::Anthropic,
        ),
        (
            "gemini/gemini-2.0-flash",
            0.10,
            0.40,
            CacheSchedule::Read(0.025),
        ),
        (
            "vertex/gemini-2.0-flash",
            0.10,
            0.40,
            CacheSchedule::Read(0.025),
        ),
        (
            "gemini/gemini-2.0-flash-lite",
            0.075,
            0.30,
            CacheSchedule::None,
        ),
        (
            "gemini/gemini-2.5-pro",
            1.25,
            10.00,
            CacheSchedule::Read(0.125),
        ),
        (
            "gemini/gemini-2.5-flash",
            0.30,
            2.50,
            CacheSchedule::Read(0.03),
        ),
        (
            "anthropic/claude-3-7-sonnet-latest",
            3.00,
            15.00,
            CacheSchedule::Anthropic,
        ),
        (
            "anthropic/claude-sonnet-4",
            3.00,
            15.00,
            CacheSchedule::Anthropic,
        ),
        (
            "anthropic/claude-sonnet-4-5",
            3.00,
            15.00,
            CacheSchedule::Anthropic,
        ),
        (
            "anthropic/claude-sonnet-4-6",
            3.00,
            15.00,
            CacheSchedule::Anthropic,
        ),
        (
            "anthropic/claude-haiku-4-5",
            1.00,
            5.00,
            CacheSchedule::Anthropic,
        ),
        (
            "anthropic/claude-opus-4",
            15.00,
            75.00,
            CacheSchedule::Anthropic,
        ),
        (
            "groq/llama-3.3-70b-versatile",
            0.59,
            0.79,
            CacheSchedule::None,
        ),
        ("groq/llama-3.1-8b-instant", 0.05, 0.08, CacheSchedule::None),
        ("groq/mixtral-8x7b-32768", 0.24, 0.24, CacheSchedule::None),
        ("deepseek/deepseek-chat", 0.27, 1.10, CacheSchedule::None),
        (
            "deepseek/deepseek-reasoner",
            0.55,
            2.19,
            CacheSchedule::None,
        ),
        (
            "mistral/mistral-small-latest",
            0.20,
            0.60,
            CacheSchedule::None,
        ),
        (
            "mistral/mistral-large-latest",
            2.00,
            6.00,
            CacheSchedule::None,
        ),
        (
            "together/meta-llama/Llama-3.3-70B-Instruct-Turbo",
            0.88,
            0.88,
            CacheSchedule::None,
        ),
        (
            "fireworks/accounts/fireworks/models/llama-v3p3-70b-instruct",
            0.90,
            0.90,
            CacheSchedule::None,
        ),
        ("cerebras/llama-3.3-70b", 0.85, 1.20, CacheSchedule::None),
        ("xai/grok-3-mini", 0.30, 0.50, CacheSchedule::None),
        ("xai/grok-4", 3.00, 15.00, CacheSchedule::None),
        ("perplexity/sonar", 1.00, 1.00, CacheSchedule::None),
        ("perplexity/sonar-pro", 3.00, 15.00, CacheSchedule::None),
        ("cohere/command-a-03-2025", 2.50, 10.00, CacheSchedule::None),
        (
            "cohere/command-r-plus-08-2024",
            2.50,
            10.00,
            CacheSchedule::None,
        ),
        (
            "openrouter/openai/gpt-4o-mini",
            0.15,
            0.60,
            CacheSchedule::Read(0.075),
        ),
        (
            "deepinfra/meta-llama/Meta-Llama-3.1-8B-Instruct",
            0.03,
            0.04,
            CacheSchedule::None,
        ),
        (
            "bedrock/anthropic.claude-3-5-haiku-20241022-v1:0",
            0.80,
            4.00,
            CacheSchedule::Anthropic,
        ),
        (
            "hyperbolic/meta-llama/Meta-Llama-3.1-70B-Instruct",
            0.10,
            0.40,
            CacheSchedule::None,
        ),
    ];
    table
        .iter()
        .find(|(k, _, _, _)| k.eq_ignore_ascii_case(&key))
        .map(|(_, input, output, schedule)| listed_rates(*input, *output, schedule))
}

fn listed_rates(input: f64, output: f64, schedule: &CacheSchedule) -> TokenRates {
    match schedule {
        CacheSchedule::None => TokenRates {
            input,
            output,
            cache_read: None,
            cache_write: None,
            cache_write_1h: None,
        },
        CacheSchedule::Read(cache_read) => TokenRates {
            input,
            output,
            cache_read: Some(*cache_read),
            cache_write: None,
            cache_write_1h: None,
        },
        CacheSchedule::Anthropic => TokenRates {
            input,
            output,
            cache_read: Some(input * 0.1),
            cache_write: Some(input * 1.25),
            cache_write_1h: Some(input * 2.0),
        },
    }
}

/// Cost in USD for one round.
///
/// Cache buckets are clamped so they cannot exceed `prompt` or overlap.
/// A bucket whose rate is `None` is billed at the uncached input rate.
///
/// ```
/// use llmrace::cost::{usage_cost_usd, token_rates, UsageTokens};
///
/// let rates = token_rates("openai", "gpt-4o-mini").unwrap();
/// let cost = usage_cost_usd(
///     rates,
///     UsageTokens {
///         prompt: 1_000.0,
///         cache_read: 800.0,
///         cache_write: 0.0,
///         cache_write_1h: 0.0,
///         output: 100.0,
///     },
/// );
/// // 200 fresh × $0.15 + 800 cached × $0.075 + 100 output × $0.60, per million.
/// assert!((cost - 0.00015).abs() < 1e-12, "{cost}");
/// ```
#[must_use]
pub fn usage_cost_usd(rates: TokenRates, usage: UsageTokens) -> f64 {
    let prompt = usage.prompt.max(0.0);
    let cache_read = usage.cache_read.max(0.0).min(prompt);
    let cache_write = usage.cache_write.max(0.0).min(prompt - cache_read);
    let cache_write_1h = usage
        .cache_write_1h
        .max(0.0)
        .min(prompt - cache_read - cache_write);
    let fresh = prompt - cache_read - cache_write - cache_write_1h;
    let read_rate = rates.cache_read.unwrap_or(rates.input);
    let write_rate = rates.cache_write.unwrap_or(rates.input);
    let write_1h_rate = rates.cache_write_1h.unwrap_or(rates.input);
    (fresh * rates.input
        + cache_read * read_rate
        + cache_write * write_rate
        + cache_write_1h * write_1h_rate
        + usage.output.max(0.0) * rates.output)
        / 1_000_000.0
}

/// `true` when every non-zero cache bucket has a published rate.
///
/// A zero bucket does not need a rate. Callers use this to avoid presenting
/// an input-rate fallback as a cache discount.
#[must_use]
pub fn cache_rates_cover(rates: TokenRates, usage: UsageTokens) -> bool {
    (usage.cache_read <= 0.0 || rates.cache_read.is_some())
        && (usage.cache_write <= 0.0 || rates.cache_write.is_some())
        && (usage.cache_write_1h <= 0.0 || rates.cache_write_1h.is_some())
}

/// Estimates prompt token count as the maximum of a word-based and a
/// character-based heuristic.
///
/// ```
/// use llmrace::cost::estimate_prompt_tokens;
///
/// let tokens = estimate_prompt_tokens("Explain zero-knowledge proofs briefly.");
/// assert!(tokens > 3.0 && tokens < 20.0, "got {tokens}");
/// ```
#[must_use]
pub fn estimate_prompt_tokens(prompt: &str) -> f64 {
    let words = prompt.split_whitespace().count() as f64;
    let chars = prompt.chars().count() as f64;
    let by_words = words * 1.33;
    let by_chars = chars / 4.0;
    by_words.max(by_chars)
}

/// Formats a USD amount for compact display in reports.
///
/// ```
/// use llmrace::cost::format_usd;
///
/// assert_eq!(format_usd(0.000234), "$0.000234");
/// assert_eq!(format_usd(1.5), "$1.500");
/// ```
#[must_use]
pub fn format_usd(v: f64) -> String {
    if v == 0.0 {
        "0".to_string()
    } else if v < 0.01 {
        format!("${:.6}", v)
    } else {
        format!("${:.3}", v)
    }
}
