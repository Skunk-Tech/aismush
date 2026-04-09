use hyper::HeaderMap;
use serde_json::Value;

/// Which AI agent client is making the request.
#[derive(Debug, Clone, PartialEq)]
pub enum ClientKind {
    ClaudeCode,
    Goose,
    Unknown(String),
}

impl std::fmt::Display for ClientKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientKind::ClaudeCode => f.write_str("claude-code"),
            ClientKind::Goose => f.write_str("goose"),
            ClientKind::Unknown(ua) => write!(f, "unknown({})", truncate(ua, 30)),
        }
    }
}

/// API format of the incoming request.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InboundFormat {
    /// Anthropic Messages API: content blocks with type fields, tool_use/tool_result
    Anthropic,
    /// OpenAI Chat Completions API: role/content strings, tool_calls, function_call
    OpenAI,
}

/// Detected client information for a single request.
#[derive(Debug, Clone)]
pub struct ClientInfo {
    pub kind: ClientKind,
    pub inbound_format: InboundFormat,
}

/// Detect the client kind and inbound API format from request metadata.
pub fn detect(headers: &HeaderMap, body: &Option<Value>, path: &str) -> ClientInfo {
    let format = detect_format(headers, body, path);
    let kind = detect_kind(headers);
    ClientInfo { kind, inbound_format: format }
}

fn detect_format(_headers: &HeaderMap, body: &Option<Value>, path: &str) -> InboundFormat {
    // 1. Path-based detection (most reliable)
    if path.contains("/chat/completions") {
        return InboundFormat::OpenAI;
    }
    if path.contains("/messages") {
        return InboundFormat::Anthropic;
    }

    // 2. Body structure detection
    if let Some(body) = body {
        // OpenAI uses "functions" or "tools" with "function" sub-objects
        if body.get("functions").is_some() {
            return InboundFormat::OpenAI;
        }

        // Check message structure
        if let Some(messages) = body.get("messages").and_then(|m| m.as_array()) {
            for msg in messages {
                // OpenAI: tool_calls array on assistant messages
                if msg.get("tool_calls").is_some() {
                    return InboundFormat::OpenAI;
                }
                // OpenAI: role "tool" for tool results
                if msg.get("role").and_then(|r| r.as_str()) == Some("tool") {
                    return InboundFormat::OpenAI;
                }
                // Anthropic: content is array of typed blocks
                if let Some(content) = msg.get("content").and_then(|c| c.as_array()) {
                    if content.iter().any(|b| b.get("type").is_some()) {
                        return InboundFormat::Anthropic;
                    }
                }
            }
        }

        // Anthropic: has "system" as top-level field (not a message)
        if body.get("system").is_some() && body.get("model").is_some() {
            return InboundFormat::Anthropic;
        }
    }

    // Default: assume Anthropic (backward compat with Claude Code)
    InboundFormat::Anthropic
}

fn detect_kind(headers: &HeaderMap) -> ClientKind {
    let ua = headers
        .get("user-agent")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    // Claude Code sends a distinctive user-agent
    if ua.contains("claude-code") || ua.contains("ClaudeCode") || ua.contains("anthropic-cli") {
        return ClientKind::ClaudeCode;
    }

    // Goose agent
    if ua.contains("goose") || ua.contains("Goose") || ua.contains("block/goose") {
        return ClientKind::Goose;
    }

    // Anthropic SDK (likely Claude Code or direct SDK usage)
    if ua.contains("anthropic") {
        return ClientKind::ClaudeCode;
    }

    if ua.is_empty() {
        // No user-agent — likely Claude Code (it sometimes omits UA through proxy)
        return ClientKind::ClaudeCode;
    }

    ClientKind::Unknown(ua.to_string())
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max { s } else { &s[..max] }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_format_by_path() {
        let headers = HeaderMap::new();
        let body = None;

        let info = detect(&headers, &body, "/v1/chat/completions");
        assert_eq!(info.inbound_format, InboundFormat::OpenAI);

        let info = detect(&headers, &body, "/v1/messages");
        assert_eq!(info.inbound_format, InboundFormat::Anthropic);
    }

    #[test]
    fn test_detect_format_by_body_structure() {
        let headers = HeaderMap::new();

        // OpenAI body with tool_calls
        let openai_body: Value = serde_json::json!({
            "model": "gpt-4",
            "messages": [
                {"role": "assistant", "content": "hello", "tool_calls": [{"id": "1", "function": {"name": "read"}}]}
            ]
        });
        let info = detect(&headers, &Some(openai_body), "/api/proxy");
        assert_eq!(info.inbound_format, InboundFormat::OpenAI);

        // Anthropic body with content blocks
        let anthropic_body: Value = serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "system": "You are helpful.",
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": "hello"}]}
            ]
        });
        let info = detect(&headers, &Some(anthropic_body), "/api/proxy");
        assert_eq!(info.inbound_format, InboundFormat::Anthropic);
    }

    #[test]
    fn test_detect_format_openai_tool_role() {
        let headers = HeaderMap::new();
        let body: Value = serde_json::json!({
            "model": "gpt-4",
            "messages": [
                {"role": "tool", "tool_call_id": "call_1", "content": "result"}
            ]
        });
        let info = detect(&headers, &Some(body), "/api/proxy");
        assert_eq!(info.inbound_format, InboundFormat::OpenAI);
    }

    #[test]
    fn test_detect_kind_from_ua() {
        let mut headers = HeaderMap::new();
        headers.insert("user-agent", "claude-code/1.0".parse().unwrap());
        let info = detect(&headers, &None, "/v1/messages");
        assert_eq!(info.kind, ClientKind::ClaudeCode);

        let mut headers = HeaderMap::new();
        headers.insert("user-agent", "goose/0.9".parse().unwrap());
        let info = detect(&headers, &None, "/v1/messages");
        assert_eq!(info.kind, ClientKind::Goose);
    }

    #[test]
    fn test_default_is_anthropic() {
        let headers = HeaderMap::new();
        let info = detect(&headers, &None, "/api/unknown");
        assert_eq!(info.inbound_format, InboundFormat::Anthropic);
        assert_eq!(info.kind, ClientKind::ClaudeCode); // no UA → assume Claude Code
    }
}
