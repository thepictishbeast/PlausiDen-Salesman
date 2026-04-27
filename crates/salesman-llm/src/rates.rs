//! Per-model rate table. Used by the cost ledger to convert
//! (prompt_tokens, output_tokens) → micro-USD.
//!
//! Rates are PER-MILLION-TOKENS in micro-USD (so 1 USD/M tokens =
//! 1_000_000 micro-USD per million tokens). Update when Anthropic /
//! Google publish new pricing.
//!
//! Maintained against published rates as of 2026-04. If a model
//! isn't in the table, cost is recorded as 0 — the LLM call still
//! works, the report just under-counts. We log a warning so the
//! operator knows to add the rate.

use crate::types::BackendKind;
use tracing::warn;

#[derive(Debug, Clone, Copy)]
pub struct Rate {
    pub backend: BackendKind,
    pub model: &'static str,
    pub input_per_million_micro_usd: u64,
    pub output_per_million_micro_usd: u64,
    /// Cache-hit input rate (Claude prompt caching pricing). For
    /// Gemini we treat cache hits as full-price input until they
    /// publish cached-content rates uniformly.
    pub cache_hit_per_million_micro_usd: u64,
}

/// Hand-curated table. KEEP IN SYNC with vendor pricing pages.
const RATES: &[Rate] = &[
    // Claude — pricing as of 2026-04 (per million tokens, USD).
    Rate {
        backend: BackendKind::Claude,
        model: "claude-opus-4-7",
        input_per_million_micro_usd: 15_000_000,   // $15/M
        output_per_million_micro_usd: 75_000_000,  // $75/M
        cache_hit_per_million_micro_usd: 1_500_000, // $1.50/M (10% of input)
    },
    Rate {
        backend: BackendKind::Claude,
        model: "claude-sonnet-4-6",
        input_per_million_micro_usd: 3_000_000,    // $3/M
        output_per_million_micro_usd: 15_000_000,  // $15/M
        cache_hit_per_million_micro_usd: 300_000,  // $0.30/M
    },
    Rate {
        backend: BackendKind::Claude,
        model: "claude-haiku-4-5-20251001",
        input_per_million_micro_usd: 1_000_000,    // $1/M
        output_per_million_micro_usd: 5_000_000,   // $5/M
        cache_hit_per_million_micro_usd: 100_000,  // $0.10/M
    },
    // Gemini — pricing as of 2026-04 (per million tokens, USD).
    Rate {
        backend: BackendKind::Gemini,
        model: "gemini-1.5-pro",
        input_per_million_micro_usd: 1_250_000,    // $1.25/M (≤128k context)
        output_per_million_micro_usd: 5_000_000,   // $5/M
        cache_hit_per_million_micro_usd: 1_250_000,
    },
    Rate {
        backend: BackendKind::Gemini,
        model: "gemini-1.5-flash",
        input_per_million_micro_usd: 75_000,        // $0.075/M
        output_per_million_micro_usd: 300_000,      // $0.30/M
        cache_hit_per_million_micro_usd: 75_000,
    },
    // LFI is sovereign / self-hosted. Treat as zero-cost for the ledger.
    Rate {
        backend: BackendKind::Lfi,
        model: "lfi",
        input_per_million_micro_usd: 0,
        output_per_million_micro_usd: 0,
        cache_hit_per_million_micro_usd: 0,
    },
];

/// Look up a rate by (backend, model). Returns `None` if not in
/// table; caller treats as zero-cost + logs.
pub fn lookup_rate(backend: BackendKind, model: &str) -> Option<Rate> {
    RATES
        .iter()
        .find(|r| r.backend == backend && r.model == model)
        .copied()
}

/// Compute cost in micro-USD for one call.
pub fn compute_cost_micro_usd(
    backend: BackendKind,
    model: &str,
    prompt_tokens: u32,
    output_tokens: u32,
    cache_hit_tokens: u32,
) -> u64 {
    let Some(rate) = lookup_rate(backend, model) else {
        warn!(%backend, %model, "no rate in table; cost recorded as 0");
        return 0;
    };
    // Input tokens = prompt minus the cache-hit subset. The cache-hit
    // portion is billed at the cache-hit rate.
    let billed_input_tokens = prompt_tokens.saturating_sub(cache_hit_tokens) as u64;
    let billed_cache_tokens = cache_hit_tokens as u64;
    let billed_output_tokens = output_tokens as u64;

    let input_cost = (billed_input_tokens * rate.input_per_million_micro_usd) / 1_000_000;
    let cache_cost = (billed_cache_tokens * rate.cache_hit_per_million_micro_usd) / 1_000_000;
    let output_cost = (billed_output_tokens * rate.output_per_million_micro_usd) / 1_000_000;
    input_cost + cache_cost + output_cost
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_model_costs_correctly() {
        // Sonnet: 1k input + 1k output = 0.001*$3 + 0.001*$15 = $0.018 = 18_000 micro.
        let cost = compute_cost_micro_usd(BackendKind::Claude, "claude-sonnet-4-6", 1000, 1000, 0);
        assert_eq!(cost, 18_000);
    }

    #[test]
    fn cache_hit_discounted() {
        // Sonnet: prompt 2000 of which 1000 are cache hit. Output 0.
        // Billed: 1000 input @ $3/M = 3_000 micro
        //         1000 cache @ $0.30/M = 300 micro
        // Total = 3_300 micro.
        let cost = compute_cost_micro_usd(BackendKind::Claude, "claude-sonnet-4-6", 2000, 0, 1000);
        assert_eq!(cost, 3_300);
    }

    #[test]
    fn unknown_model_zero_cost() {
        let cost = compute_cost_micro_usd(BackendKind::Claude, "claude-fake-99", 1000, 1000, 0);
        assert_eq!(cost, 0);
    }
}
