//! Per-model list prices, for the cost estimate. The one place to edit when
//! prices change.
//!
//! Source: Anthropic's published API list prices (the model pricing table in
//! the Claude API docs), USD per million tokens, as cached 2026-10-06.
//! Input and output are the listed rates. Cache reads use the listed cache-read
//! rate where one was published (Fable 5.1, Opus 5.5, Sonnet 5.5) and 0.1x
//! input otherwise. Cache writes are 1.25x input (the 5-minute rate; 1-hour
//! writes cost more, and transcripts don't say which was used).
//! Haiku 5.5 is priced at its rate for prompts up to 100K tokens; longer
//! prompts cost more, so its cost is a slight underestimate.
//!
//! This is an estimate: subscription plans, batch/fast-mode/priority
//! multipliers, and discounts are not modelled. A model that is not listed
//! here has no price: its tokens are counted but its cost is not, and the
//! result is marked partial.

use crate::usage::Tokens;

/// USD per million tokens.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Price {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
}

const fn price(input: f64, output: f64, cache_read: f64) -> Price {
    Price { input, output, cache_read, cache_write: input * 1.25 }
}

/// Model ids, most specific first. An id matches when it equals the key or
/// continues it with a date stamp (`-20251101`) or a bracketed suffix
/// (`[1m]`). Aliases like "opus" are not here: they don't say which version.
const PRICES: &[(&str, Price)] = &[
    ("claude-fable-5-1", price(10.0, 50.0, 0.25)),
    ("claude-mythos-5-1", price(10.0, 50.0, 0.25)),
    ("claude-fable-5", price(10.0, 50.0, 1.0)),
    ("claude-opus-5-5", price(4.0, 20.0, 0.20)),
    ("claude-opus-5", price(5.0, 25.0, 0.50)),
    ("claude-opus-4-8", price(5.0, 25.0, 0.50)),
    ("claude-opus-4-7", price(5.0, 25.0, 0.50)),
    ("claude-opus-4-6", price(5.0, 25.0, 0.50)),
    ("claude-sonnet-5-5", price(2.0, 10.0, 0.20)),
    ("claude-sonnet-5", price(2.0, 10.0, 0.20)),
    ("claude-sonnet-4-6", price(3.0, 15.0, 0.30)),
    ("claude-haiku-5-5", price(0.10, 0.50, 0.01)),
    ("claude-haiku-4-5", price(1.0, 5.0, 0.10)),
];

pub fn price_of(model: &str) -> Option<Price> {
    let m = model.trim().to_ascii_lowercase();
    PRICES.iter().find(|(key, _)| continues(&m, key)).map(|(_, p)| *p)
}

/// `m` is `key`, `key-<8 digit date>`, or `key[...]`.
fn continues(m: &str, key: &str) -> bool {
    let Some(rest) = m.strip_prefix(key) else { return false };
    rest.is_empty()
        || rest.starts_with('[')
        || rest.strip_prefix('-').is_some_and(|d| d.len() == 8 && d.bytes().all(|b| b.is_ascii_digit()))
}

/// Estimated cost in USD of `t` on `model`, or `None` for a model without a price.
pub fn cost(model: &str, t: &Tokens) -> Option<f64> {
    let p = price_of(model)?;
    Some((t.input as f64 * p.input + t.output as f64 * p.output + t.cache_read as f64 * p.cache_read + t.cache_creation as f64 * p.cache_write) / 1e6)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_exact_dated_and_bracketed_ids_but_not_prefixes_of_other_models() {
        assert_eq!(price_of("claude-opus-5-5").unwrap().input, 4.0);
        assert_eq!(price_of("claude-opus-5-5-20261001").unwrap().input, 4.0);
        assert_eq!(price_of("claude-opus-5-5[1m]").unwrap().input, 4.0);
        assert_eq!(price_of("claude-opus-5").unwrap().input, 5.0);
        assert_eq!(price_of("Claude-Haiku-5-5").unwrap().output, 0.5);
        assert!(price_of("opus").is_none());
        assert!(price_of("<synthetic>").is_none());
        assert!(price_of("claude-opus-5-6").is_none());
    }

    #[test]
    fn cost_adds_all_four_kinds() {
        let t = Tokens { input: 1_000_000, output: 1_000_000, cache_read: 1_000_000, cache_creation: 1_000_000 };
        let c = cost("claude-sonnet-5-5", &t).unwrap();
        assert!((c - (2.0 + 10.0 + 0.2 + 2.5)).abs() < 1e-9, "{c}");
        assert_eq!(cost("mystery", &t), None);
    }
}
