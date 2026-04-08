/// Cost calculation based on provider pricing.

pub struct CostResult {
    pub actual_cost: f64,
    pub claude_equiv_cost: f64,
    pub savings: f64,
}

/// Calculate cost for a request.
///
/// `input_tokens`/`output_tokens` are the actual token counts (post-compression).
/// To calculate what the request would have cost WITHOUT AISmush, use `calculate_with_compression`.
pub fn calculate(provider: &str, model: &str, input_tokens: u64, output_tokens: u64) -> CostResult {
    calculate_with_compression(provider, model, input_tokens, output_tokens, 0)
}

/// Calculate cost including compression savings in the "without AISmush" baseline.
///
/// `tokens_saved_by_compression` is the number of input tokens that were compressed away
/// BEFORE the request reached the provider. These need to be added back to compute the
/// true "what would this have cost without the proxy" number.
pub fn calculate_with_compression(
    provider: &str,
    model: &str,
    input_tokens: u64,
    output_tokens: u64,
    tokens_saved_by_compression: u64,
) -> CostResult {
    let pricing = get_pricing(provider, model);
    let actual = (input_tokens as f64 / 1_000_000.0) * pricing.0
        + (output_tokens as f64 / 1_000_000.0) * pricing.1;

    // "Without AISmush" baseline = actual + what we saved.
    //
    // Two sources of savings:
    // 1. Compression: tokens we removed, priced at THIS model's rate
    //    (what these tokens would have cost on whatever model we're using)
    // 2. Routing: if using a cheaper model than Claude Sonnet, the price difference
    //    (only applies to non-Claude providers)
    //
    // equiv = actual + compression_savings + routing_savings
    // This guarantees equiv >= actual (savings are never negative).

    let compression_savings = (tokens_saved_by_compression as f64 / 1_000_000.0) * pricing.0;

    let routing_savings = if provider != "claude" {
        let claude_pricing = get_pricing("claude", "claude-sonnet-4-20250514");
        let claude_cost = (input_tokens as f64 / 1_000_000.0) * claude_pricing.0
            + (output_tokens as f64 / 1_000_000.0) * claude_pricing.1;
        (claude_cost - actual).max(0.0)
    } else {
        0.0
    };

    let total_savings = compression_savings + routing_savings;
    let equiv = actual + total_savings;

    CostResult {
        actual_cost: actual,
        claude_equiv_cost: equiv,
        savings: total_savings,
    }
}

/// Get (input_price_per_M, output_price_per_M) for a model.
pub fn get_pricing(provider: &str, model: &str) -> (f64, f64) {
    let m = model.to_lowercase();

    // Local models are free
    if provider.starts_with("local-") {
        return (0.0, 0.0);
    }

    if provider == "deepseek" || m.contains("deepseek") {
        if m.contains("reasoner") {
            return (0.55, 2.19);
        }
        return (0.27, 1.10); // deepseek-chat
    }

    // OpenRouter — try to match by model name
    if provider == "openrouter" || provider.starts_with("openrouter") {
        if m.contains("deepseek") { return (0.27, 1.10); }
        if m.contains("llama") && m.contains("405") { return (0.80, 0.80); }
        if m.contains("llama") && m.contains("70") { return (0.40, 0.40); }
        if m.contains("llama") { return (0.05, 0.05); }
        if m.contains("mistral") && m.contains("large") { return (2.00, 6.00); }
        if m.contains("mistral") { return (0.15, 0.15); }
        if m.contains("gpt-4o-mini") { return (0.15, 0.60); }
        if m.contains("gpt-4o") { return (2.50, 10.00); }
        if m.contains("gemini") && m.contains("pro") { return (1.25, 5.00); }
        if m.contains("gemini") && m.contains("flash") { return (0.075, 0.30); }
        if m.contains("qwen") { return (0.15, 0.60); }
        return (0.50, 1.00);
    }

    // Claude models
    if m.contains("opus") { return (15.00, 75.00); }
    if m.contains("sonnet") { return (3.00, 15.00); }
    if m.contains("haiku") { return (0.80, 4.00); }

    // Default to Sonnet pricing
    (3.00, 15.00)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deepseek_routing_savings() {
        let r = calculate("deepseek", "deepseek-chat", 10_000, 1_000);
        assert!((r.actual_cost - 0.0038).abs() < 0.001);
        assert!(r.claude_equiv_cost > r.actual_cost);
        assert!(r.savings > 0.0);
    }

    #[test]
    fn test_claude_no_compression_no_savings() {
        let r = calculate("claude", "claude-sonnet-4-20250514", 10_000, 1_000);
        assert!(r.actual_cost > 0.0);
        assert_eq!(r.savings, 0.0);
    }

    #[test]
    fn test_claude_sonnet_with_compression() {
        // Sonnet: 5K tokens compressed away at $3/M = $0.015 savings
        let r = calculate_with_compression("claude", "claude-sonnet-4-20250514", 10_000, 1_000, 5_000);
        assert!(r.savings > 0.0);
        assert!(r.claude_equiv_cost > r.actual_cost);
        let expected = 5_000.0 * 3.0 / 1_000_000.0;
        assert!((r.savings - expected).abs() < 0.001);
    }

    #[test]
    fn test_claude_opus_with_compression_never_negative() {
        // Opus ($15/$75): 5K tokens compressed = $0.075 savings
        // Must NOT go negative
        let r = calculate_with_compression("claude", "claude-opus-4-20250514", 10_000, 1_000, 5_000);
        assert!(r.savings > 0.0);
        assert!(r.claude_equiv_cost >= r.actual_cost);
        let expected = 5_000.0 * 15.0 / 1_000_000.0;
        assert!((r.savings - expected).abs() < 0.001);
    }

    #[test]
    fn test_claude_opus_no_compression_zero_savings() {
        // Opus with no compression = zero savings (no routing savings for Claude)
        let r = calculate("claude", "claude-opus-4-20250514", 10_000, 1_000);
        assert_eq!(r.savings, 0.0);
        assert_eq!(r.claude_equiv_cost, r.actual_cost);
    }

    #[test]
    fn test_local_model_free() {
        let r = calculate("local-ollama", "qwen3:8b", 10_000, 1_000);
        assert_eq!(r.actual_cost, 0.0);
        assert!(r.claude_equiv_cost > 0.0);
        assert!(r.savings > 0.0);
    }

    #[test]
    fn test_deepseek_with_compression_saves_more() {
        let no_comp = calculate("deepseek", "deepseek-chat", 10_000, 1_000);
        let with_comp = calculate_with_compression("deepseek", "deepseek-chat", 10_000, 1_000, 5_000);
        assert!(with_comp.savings > no_comp.savings);
    }

    #[test]
    fn test_equiv_always_gte_actual() {
        // For ANY provider with ANY compression, equiv must be >= actual
        for (prov, model) in &[
            ("claude", "claude-opus-4-20250514"),
            ("claude", "claude-sonnet-4-20250514"),
            ("claude", "claude-haiku-4-5-20251001"),
            ("deepseek", "deepseek-chat"),
            ("local-ollama", "qwen3:8b"),
            ("openrouter", "deepseek/deepseek-chat"),
        ] {
            for comp in &[0u64, 1000, 10000, 50000] {
                let r = calculate_with_compression(prov, model, 10_000, 1_000, *comp);
                assert!(
                    r.claude_equiv_cost >= r.actual_cost,
                    "equiv < actual for {} {} comp={}: equiv={} actual={}",
                    prov, model, comp, r.claude_equiv_cost, r.actual_cost
                );
            }
        }
    }
}
