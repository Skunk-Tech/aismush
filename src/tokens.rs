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

/// Extract actual token usage from an SSE stream response.
/// Parses `message_start` (input tokens) and `message_delta` (output tokens).
pub fn extract_usage(stream_data: &str) -> TokenUsage {
    let mut usage = TokenUsage::default();

    // Find message_start event for input tokens
    if let Some(pos) = stream_data.find("event: message_start\n") {
        if let Some(data_line) = extract_data_line(stream_data, pos) {
            if let Ok(val) = serde_json::from_str::<Value>(data_line) {
                if let Some(u) = val.get("message").and_then(|m| m.get("usage")) {
                    usage.input_tokens = u["input_tokens"].as_u64().unwrap_or(0);
                    usage.cache_read_tokens = u["cache_read_input_tokens"].as_u64().unwrap_or(0);
                    usage.cache_write_tokens = u["cache_creation_input_tokens"].as_u64().unwrap_or(0);
                }
            }
        }
    }

    // Find message_delta event for output tokens
    if let Some(pos) = stream_data.find("event: message_delta\n") {
        if let Some(data_line) = extract_data_line(stream_data, pos) {
            if let Ok(val) = serde_json::from_str::<Value>(data_line) {
                if let Some(u) = val.get("usage") {
                    usage.output_tokens = u["output_tokens"].as_u64().unwrap_or(0);
                }
            }
        }
    }

    usage
}

/// Find the "data: {...}" line after a given position in SSE text.
fn extract_data_line(text: &str, start: usize) -> Option<&str> {
    let rest = &text[start..];
    for line in rest.lines() {
        if let Some(json) = line.strip_prefix("data: ") {
            if json.starts_with('{') {
                return Some(json);
            }
        }
    }
    None
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

    #[test]
    fn test_extract_usage() {
        let stream = r#"event: message_start
data: {"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","content":[],"model":"claude-3","usage":{"input_tokens":1234,"cache_read_input_tokens":500,"cache_creation_input_tokens":100}}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":567}}

event: message_stop
data: {"type":"message_stop"}"#;

        let usage = extract_usage(stream);
        assert_eq!(usage.input_tokens, 1234);
        assert_eq!(usage.output_tokens, 567);
        assert_eq!(usage.cache_read_tokens, 500);
        assert_eq!(usage.cache_write_tokens, 100);
    }
}
