//! Approximate cost of a turn from its token usage and list prices (US dollars per million
//! tokens). Only models with known prices get a figure; the panel shows tokens for the rest.
//! Prices change: update the table when they do.

use crate::provider::Usage;

/// List prices per million tokens.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Price {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    /// Writing a prompt-cache entry (Anthropic; free elsewhere).
    pub cache_write: f64,
}

const fn p(input: f64, output: f64, cache_read: f64, cache_write: f64) -> Price {
    Price { input, output, cache_read, cache_write }
}

/// Model id → price (Anthropic first-party API and OpenAI list prices, 2026).
const PRICES: &[(&str, Price)] = &[
    ("claude-fable-5-1", p(10.0, 50.0, 0.25, 12.5)),
    ("claude-opus-5-5", p(4.0, 20.0, 0.20, 5.0)),
    ("claude-opus-5", p(5.0, 25.0, 0.50, 6.25)),
    ("claude-sonnet-5-5", p(2.0, 10.0, 0.20, 2.5)),
    ("claude-haiku-5-5", p(0.10, 0.50, 0.01, 0.125)),
    ("gpt-5", p(1.25, 10.0, 0.125, 0.0)),
    ("gpt-5-mini", p(0.25, 2.0, 0.025, 0.0)),
    ("gpt-5-nano", p(0.05, 0.40, 0.005, 0.0)),
];

/// The price of `model` (its exact id), when known.
pub fn price(model: &str) -> Option<Price> {
    PRICES.iter().find(|(id, _)| *id == model.trim()).map(|(_, p)| *p)
}

/// What `usage` cost on `model`, in dollars, when its price is known.
pub fn cost(model: &str, usage: &Usage) -> Option<f64> {
    let p = price(model)?;
    let m = |tokens: u64, per: f64| tokens as f64 * per / 1_000_000.0;
    Some(
        m(usage.input_tokens, p.input)
            + m(usage.output_tokens, p.output)
            + m(usage.cache_read_tokens, p.cache_read)
            + m(usage.cache_write_tokens, p.cache_write),
    )
}

/// A cost for display: `$0.0042`, `$0.12`, `$3.40`.
pub fn format(dollars: f64) -> String {
    if dollars < 0.01 { format!("${dollars:.4}") } else { format!("${dollars:.2}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn costs() {
        let u = Usage { input_tokens: 1_000_000, output_tokens: 100_000, cache_read_tokens: 2_000_000, cache_write_tokens: 0 };
        let c = cost("gpt-5-mini", &u).unwrap();
        assert!((c - (0.25 + 0.2 + 0.05)).abs() < 1e-9, "{c}");
        assert!(cost("some-local-model", &u).is_none());
        assert!(price("claude-opus-5-5").is_some());
        assert_eq!(format(0.00421), "$0.0042");
        assert_eq!(format(3.4), "$3.40");
    }
}
