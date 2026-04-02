use serde_json::Value;

/// Token usage extracted from SSE stream.
#[derive(Default, Debug, Clone)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
}

/// Estimate token count from a JSON body (chars / 4 heuristic).
/// Good enough for routing decisions — within 20% of actual.
pub fn estimate_tokens(body: &Value) -> usize {
    // Serialize to get total char count, divide by 4 (average chars per token)
    let len = serde_json::to_string(body).map(|s| s.len()).unwrap_or(0);
    len / 4
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_estimate_tokens() {
        let body = serde_json::json!({"messages": [{"role": "user", "content": "hello world"}]});
        let est = estimate_tokens(&body);
        assert!(est > 5 && est < 30);
    }
}
