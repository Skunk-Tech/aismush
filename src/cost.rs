/// Cost calculation based on provider pricing.

pub struct CostResult {
    pub actual_cost: f64,
    pub claude_equiv_cost: f64,
    pub savings: f64,
}

/// Calculate cost for a request.
pub fn calculate(provider: &str, model: &str, input_tokens: u64, output_tokens: u64) -> CostResult {
    let pricing = get_pricing(provider, model);
    let actual = (input_tokens as f64 / 1_000_000.0) * pricing.0
        + (output_tokens as f64 / 1_000_000.0) * pricing.1;

    // Claude equivalent: what would Sonnet have cost?
    let equiv = if provider == "deepseek" {
        let sonnet = get_pricing("claude", "claude-sonnet-4-20250514");
        (input_tokens as f64 / 1_000_000.0) * sonnet.0
            + (output_tokens as f64 / 1_000_000.0) * sonnet.1
    } else {
        actual // Already Claude, equiv = actual
    };

    CostResult {
        actual_cost: actual,
        claude_equiv_cost: equiv,
        savings: equiv - actual,
    }
}

/// Get (input_price_per_M, output_price_per_M) for a model.
pub fn get_pricing(provider: &str, model: &str) -> (f64, f64) {
    let m = model.to_lowercase();

    if provider == "deepseek" || m.contains("deepseek") {
        if m.contains("reasoner") {
            return (0.55, 2.19);
        }
        return (0.27, 1.10); // deepseek-chat
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
    fn test_deepseek_cost() {
        let r = calculate("deepseek", "deepseek-chat", 10_000, 1_000);
        assert!((r.actual_cost - 0.0038).abs() < 0.001); // 10K*0.27/1M + 1K*1.10/1M
        assert!(r.claude_equiv_cost > r.actual_cost);
        assert!(r.savings > 0.0);
    }

    #[test]
    fn test_claude_cost() {
        let r = calculate("claude", "claude-sonnet-4-20250514", 10_000, 1_000);
        assert!(r.actual_cost > 0.0);
        assert_eq!(r.savings, 0.0); // No savings when using Claude
    }
}
