//! Bidirectional format conversion between Anthropic and OpenAI API formats.
//!
//! Claude Code sends/expects Anthropic format. All non-Claude providers
//! (OpenRouter, Ollama, LM Studio, llama.cpp, vLLM, etc.) use OpenAI-compatible format.
//! DeepSeek has an Anthropic-compatible endpoint so it doesn't need conversion.

use serde_json::{json, Value};

/// Convert an Anthropic-format request body to OpenAI /v1/chat/completions format.
pub fn anthropic_to_openai(body: &Value, model: &str) -> Value {
    let mut messages: Vec<Value> = Vec::new();

    // System prompt -> system message
    if let Some(system) = body.get("system") {
        let text = match system {
            Value::String(s) => s.clone(),
            Value::Array(arr) => {
                arr.iter()
                    .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join("\n\n")
            }
            _ => String::new(),
        };
        if !text.is_empty() {
            messages.push(json!({"role": "system", "content": text}));
        }
    }

    // Convert each message
    if let Some(msgs) = body.get("messages").and_then(|m| m.as_array()) {
        for msg in msgs {
            let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("user");

            match msg.get("content") {
                // Simple string content
                Some(Value::String(s)) => {
                    messages.push(json!({"role": role, "content": s}));
                }
                // Content block array (Anthropic format)
                Some(Value::Array(blocks)) => {
                    convert_content_blocks(role, blocks, &mut messages);
                }
                _ => {
                    messages.push(json!({"role": role, "content": ""}));
                }
            }
        }
    }

    let mut result = json!({
        "model": model,
        "messages": messages,
        "stream": true,
    });

    // Copy over max_tokens
    if let Some(max) = body.get("max_tokens").and_then(|m| m.as_u64()) {
        result["max_tokens"] = json!(max);
    }

    // Copy temperature if set
    if let Some(temp) = body.get("temperature").and_then(|t| t.as_f64()) {
        result["temperature"] = json!(temp);
    }

    // Copy tools if present (convert Anthropic tool format to OpenAI)
    if let Some(tools) = body.get("tools").and_then(|t| t.as_array()) {
        let openai_tools: Vec<Value> = tools.iter().map(|tool| {
            json!({
                "type": "function",
                "function": {
                    "name": tool.get("name").and_then(|n| n.as_str()).unwrap_or(""),
                    "description": tool.get("description").and_then(|d| d.as_str()).unwrap_or(""),
                    "parameters": tool.get("input_schema").cloned().unwrap_or(json!({})),
                }
            })
        }).collect();
        if !openai_tools.is_empty() {
            result["tools"] = json!(openai_tools);
        }
    }

    result
}

/// Convert Anthropic content blocks to OpenAI messages.
fn convert_content_blocks(role: &str, blocks: &[Value], messages: &mut Vec<Value>) {
    let mut text_parts: Vec<String> = Vec::new();
    let mut tool_calls: Vec<Value> = Vec::new();
    let mut tool_results: Vec<(String, String)> = Vec::new(); // (tool_call_id, content)

    for block in blocks {
        match block.get("type").and_then(|t| t.as_str()) {
            Some("text") => {
                if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                    text_parts.push(t.to_string());
                }
            }
            Some("tool_use") => {
                let id = block.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string();
                let name = block.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
                let input = block.get("input").cloned().unwrap_or(json!({}));
                tool_calls.push(json!({
                    "id": id,
                    "type": "function",
                    "function": {
                        "name": name,
                        "arguments": serde_json::to_string(&input).unwrap_or_default(),
                    }
                }));
            }
            Some("tool_result") => {
                let tool_use_id = block.get("tool_use_id").and_then(|i| i.as_str()).unwrap_or("").to_string();
                let content = match block.get("content") {
                    Some(Value::String(s)) => s.clone(),
                    Some(Value::Array(arr)) => {
                        arr.iter()
                            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                            .collect::<Vec<_>>()
                            .join("\n")
                    }
                    _ => String::new(),
                };
                tool_results.push((tool_use_id, content));
            }
            Some("thinking") => {
                // Skip thinking blocks — not supported in OpenAI format
            }
            _ => {
                // Unknown block type — try to extract text
                if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                    text_parts.push(t.to_string());
                }
            }
        }
    }

    // Emit assistant message with text + tool_calls
    if role == "assistant" {
        let mut msg = json!({"role": "assistant"});
        if !text_parts.is_empty() {
            msg["content"] = json!(text_parts.join("\n"));
        }
        if !tool_calls.is_empty() {
            msg["tool_calls"] = json!(tool_calls);
            // OpenAI requires content to be null (not missing) when tool_calls present
            if text_parts.is_empty() {
                msg["content"] = Value::Null;
            }
        }
        messages.push(msg);
    } else if !tool_results.is_empty() {
        // Tool results from user role -> individual "tool" role messages in OpenAI
        // But first, emit any text content as a user message
        if !text_parts.is_empty() {
            messages.push(json!({"role": "user", "content": text_parts.join("\n")}));
        }
        for (tool_call_id, content) in tool_results {
            messages.push(json!({
                "role": "tool",
                "tool_call_id": tool_call_id,
                "content": content,
            }));
        }
    } else {
        // Regular user message with text blocks
        let content = text_parts.join("\n");
        messages.push(json!({"role": role, "content": content}));
    }
}

/// State machine for converting OpenAI SSE stream to Anthropic SSE format.
pub struct OpenAIToAnthropicStream {
    started: bool,
    content_block_open: bool,
    tool_call_blocks: Vec<ToolCallState>,
    input_tokens: u64,
    output_tokens: u64,
    message_id: String,
    model: String,
}

struct ToolCallState {
    index: usize,
    id: String,
    name: String,
    arguments: String,
    block_started: bool,
}

impl OpenAIToAnthropicStream {
    pub fn new(model: &str) -> Self {
        Self {
            started: false,
            content_block_open: false,
            tool_call_blocks: Vec::new(),
            input_tokens: 0,
            output_tokens: 0,
            message_id: format!("msg_{:016x}", rand_id()),
            model: model.to_string(),
        }
    }

    /// Process a single OpenAI SSE data line and return zero or more Anthropic SSE events.
    pub fn process_chunk(&mut self, data: &str) -> Vec<String> {
        let mut events = Vec::new();

        if data == "[DONE]" {
            // Close any open content block
            if self.content_block_open {
                events.push(sse_event("content_block_stop", &json!({"type": "content_block_stop", "index": 0})));
                self.content_block_open = false;
            }
            // Close any open tool call blocks
            for tc in &self.tool_call_blocks {
                if tc.block_started {
                    events.push(sse_event("content_block_stop", &json!({"type": "content_block_stop", "index": tc.index + 1})));
                }
            }
            // message_delta with usage
            events.push(sse_event("message_delta", &json!({
                "type": "message_delta",
                "delta": {"stop_reason": "end_turn", "stop_sequence": null},
                "usage": {"output_tokens": self.output_tokens}
            })));
            events.push(sse_event("message_stop", &json!({"type": "message_stop"})));
            return events;
        }

        let Ok(chunk) = serde_json::from_str::<Value>(data) else {
            return events;
        };

        // Extract usage if present
        if let Some(usage) = chunk.get("usage") {
            if let Some(pt) = usage.get("prompt_tokens").and_then(|t| t.as_u64()) {
                self.input_tokens = pt;
            }
            if let Some(ct) = usage.get("completion_tokens").and_then(|t| t.as_u64()) {
                self.output_tokens = ct;
            }
        }

        // Emit message_start on first chunk
        if !self.started {
            self.started = true;
            events.push(sse_event("message_start", &json!({
                "type": "message_start",
                "message": {
                    "id": self.message_id,
                    "type": "message",
                    "role": "assistant",
                    "content": [],
                    "model": self.model,
                    "stop_reason": null,
                    "stop_sequence": null,
                    "usage": {
                        "input_tokens": self.input_tokens,
                        "cache_read_input_tokens": 0,
                        "cache_creation_input_tokens": 0,
                        "output_tokens": 0,
                    }
                }
            })));
        }

        let choices = chunk.get("choices").and_then(|c| c.as_array());
        let Some(choices) = choices else { return events; };

        for choice in choices {
            let delta = choice.get("delta");
            let Some(delta) = delta else { continue };

            // Handle text content
            if let Some(content) = delta.get("content").and_then(|c| c.as_str()) {
                if !content.is_empty() {
                    if !self.content_block_open {
                        self.content_block_open = true;
                        events.push(sse_event("content_block_start", &json!({
                            "type": "content_block_start",
                            "index": 0,
                            "content_block": {"type": "text", "text": ""}
                        })));
                    }
                    events.push(sse_event("content_block_delta", &json!({
                        "type": "content_block_delta",
                        "index": 0,
                        "delta": {"type": "text_delta", "text": content}
                    })));
                }
            }

            // Handle tool calls
            if let Some(tool_calls) = delta.get("tool_calls").and_then(|t| t.as_array()) {
                for tc in tool_calls {
                    let idx = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;

                    // Ensure we have state for this tool call
                    while self.tool_call_blocks.len() <= idx {
                        self.tool_call_blocks.push(ToolCallState {
                            index: self.tool_call_blocks.len(),
                            id: String::new(),
                            name: String::new(),
                            arguments: String::new(),
                            block_started: false,
                        });
                    }

                    let state = &mut self.tool_call_blocks[idx];

                    if let Some(id) = tc.get("id").and_then(|i| i.as_str()) {
                        state.id = id.to_string();
                    }
                    if let Some(func) = tc.get("function") {
                        if let Some(name) = func.get("name").and_then(|n| n.as_str()) {
                            state.name = name.to_string();
                        }
                        if let Some(args) = func.get("arguments").and_then(|a| a.as_str()) {
                            state.arguments.push_str(args);
                        }
                    }

                    // Start the block if we have the name and haven't started yet
                    if !state.block_started && !state.name.is_empty() {
                        // Close text block first if open
                        if self.content_block_open {
                            events.push(sse_event("content_block_stop", &json!({"type": "content_block_stop", "index": 0})));
                            self.content_block_open = false;
                        }

                        state.block_started = true;
                        let block_index = idx + 1; // text is index 0
                        events.push(sse_event("content_block_start", &json!({
                            "type": "content_block_start",
                            "index": block_index,
                            "content_block": {
                                "type": "tool_use",
                                "id": state.id,
                                "name": state.name,
                                "input": {}
                            }
                        })));
                    }

                    // Stream argument fragments
                    if let Some(func) = tc.get("function") {
                        if let Some(args) = func.get("arguments").and_then(|a| a.as_str()) {
                            if !args.is_empty() && state.block_started {
                                let block_index = idx + 1;
                                events.push(sse_event("content_block_delta", &json!({
                                    "type": "content_block_delta",
                                    "index": block_index,
                                    "delta": {"type": "input_json_delta", "partial_json": args}
                                })));
                            }
                        }
                    }
                }
            }

            // Handle finish_reason
            if let Some(reason) = choice.get("finish_reason").and_then(|r| r.as_str()) {
                let stop_reason = match reason {
                    "stop" => "end_turn",
                    "tool_calls" => "tool_use",
                    "length" => "max_tokens",
                    other => other,
                };

                // Close open blocks
                if self.content_block_open {
                    events.push(sse_event("content_block_stop", &json!({"type": "content_block_stop", "index": 0})));
                    self.content_block_open = false;
                }
                for tc in &self.tool_call_blocks {
                    if tc.block_started {
                        events.push(sse_event("content_block_stop", &json!({"type": "content_block_stop", "index": tc.index + 1})));
                    }
                }

                events.push(sse_event("message_delta", &json!({
                    "type": "message_delta",
                    "delta": {"stop_reason": stop_reason, "stop_sequence": null},
                    "usage": {"output_tokens": self.output_tokens}
                })));
            }
        }

        events
    }

    pub fn input_tokens(&self) -> u64 { self.input_tokens }
    pub fn output_tokens(&self) -> u64 { self.output_tokens }
}

fn sse_event(event_type: &str, data: &Value) -> String {
    format!("event: {}\ndata: {}\n\n", event_type, data)
}

fn rand_id() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let t = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    t.as_nanos() as u64 ^ (std::process::id() as u64) << 32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_text_conversion() {
        let body = json!({
            "model": "claude-sonnet-4-20250514",
            "system": "You are helpful.",
            "messages": [
                {"role": "user", "content": "Hello"},
            ],
            "max_tokens": 1024,
        });

        let result = anthropic_to_openai(&body, "deepseek-chat");
        let messages = result["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "You are helpful.");
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[1]["content"], "Hello");
        assert_eq!(result["model"], "deepseek-chat");
        assert_eq!(result["stream"], true);
    }

    #[test]
    fn test_content_blocks_conversion() {
        let body = json!({
            "model": "claude-sonnet-4-20250514",
            "messages": [
                {"role": "user", "content": [
                    {"type": "text", "text": "Read this file"},
                    {"type": "tool_result", "tool_use_id": "call_123", "content": "file contents here"},
                ]},
            ],
            "max_tokens": 1024,
        });

        let result = anthropic_to_openai(&body, "qwen3:8b");
        let messages = result["messages"].as_array().unwrap();
        // tool_result becomes a "tool" role message
        assert!(messages.iter().any(|m| m["role"] == "tool"));
    }

    #[test]
    fn test_tool_use_conversion() {
        let body = json!({
            "model": "claude-sonnet-4-20250514",
            "messages": [
                {"role": "assistant", "content": [
                    {"type": "text", "text": "Let me read that file."},
                    {"type": "tool_use", "id": "call_456", "name": "read_file", "input": {"path": "/src/main.rs"}},
                ]},
            ],
            "max_tokens": 1024,
        });

        let result = anthropic_to_openai(&body, "test-model");
        let messages = result["messages"].as_array().unwrap();
        assert_eq!(messages[0]["role"], "assistant");
        assert_eq!(messages[0]["content"], "Let me read that file.");
        let tool_calls = messages[0]["tool_calls"].as_array().unwrap();
        assert_eq!(tool_calls[0]["function"]["name"], "read_file");
    }

    #[test]
    fn test_tools_definition_conversion() {
        let body = json!({
            "model": "claude-sonnet-4-20250514",
            "messages": [{"role": "user", "content": "test"}],
            "tools": [{
                "name": "get_weather",
                "description": "Get weather",
                "input_schema": {"type": "object", "properties": {"city": {"type": "string"}}},
            }],
            "max_tokens": 1024,
        });

        let result = anthropic_to_openai(&body, "test-model");
        let tools = result["tools"].as_array().unwrap();
        assert_eq!(tools[0]["type"], "function");
        assert_eq!(tools[0]["function"]["name"], "get_weather");
    }

    #[test]
    fn test_thinking_blocks_stripped() {
        let body = json!({
            "model": "claude-sonnet-4-20250514",
            "messages": [
                {"role": "assistant", "content": [
                    {"type": "thinking", "thinking": "Let me think..."},
                    {"type": "text", "text": "The answer is 42."},
                ]},
            ],
            "max_tokens": 1024,
        });

        let result = anthropic_to_openai(&body, "test-model");
        let messages = result["messages"].as_array().unwrap();
        // Should only have the text, not the thinking block
        assert_eq!(messages[0]["content"], "The answer is 42.");
    }

    #[test]
    fn test_stream_simple_text() {
        let mut stream = OpenAIToAnthropicStream::new("test-model");

        // First chunk with content
        let events = stream.process_chunk(r#"{"choices":[{"delta":{"content":"Hello"},"index":0}]}"#);
        assert!(events.iter().any(|e| e.contains("message_start")));
        assert!(events.iter().any(|e| e.contains("content_block_start")));
        assert!(events.iter().any(|e| e.contains("text_delta") && e.contains("Hello")));

        // More content
        let events = stream.process_chunk(r#"{"choices":[{"delta":{"content":" world"},"index":0}]}"#);
        assert!(events.iter().any(|e| e.contains("text_delta") && e.contains(" world")));
        assert!(!events.iter().any(|e| e.contains("message_start"))); // only once

        // Done
        let events = stream.process_chunk("[DONE]");
        assert!(events.iter().any(|e| e.contains("content_block_stop")));
        assert!(events.iter().any(|e| e.contains("message_stop")));
    }

    #[test]
    fn test_stream_with_finish_reason() {
        let mut stream = OpenAIToAnthropicStream::new("test-model");

        stream.process_chunk(r#"{"choices":[{"delta":{"content":"Hi"},"index":0}]}"#);
        let events = stream.process_chunk(r#"{"choices":[{"delta":{},"finish_reason":"stop","index":0}],"usage":{"prompt_tokens":10,"completion_tokens":5}}"#);

        assert!(events.iter().any(|e| e.contains("content_block_stop")));
        assert!(events.iter().any(|e| e.contains("message_delta") && e.contains("end_turn")));
    }

    #[test]
    fn test_stream_tool_call() {
        let mut stream = OpenAIToAnthropicStream::new("test-model");

        // Tool call start
        let events = stream.process_chunk(r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"read_file","arguments":""}}]},"index":0}]}"#);
        assert!(events.iter().any(|e| e.contains("content_block_start") && e.contains("tool_use")));

        // Tool call arguments streaming
        let events = stream.process_chunk(r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"path\":"}}]},"index":0}]}"#);
        assert!(events.iter().any(|e| e.contains("input_json_delta")));

        let events = stream.process_chunk(r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"/src/main.rs\"}"}}]},"index":0}]}"#);
        assert!(events.iter().any(|e| e.contains("input_json_delta")));
    }

    #[test]
    fn test_system_array_format() {
        let body = json!({
            "model": "claude-sonnet-4-20250514",
            "system": [
                {"type": "text", "text": "Part 1"},
                {"type": "text", "text": "Part 2"},
            ],
            "messages": [{"role": "user", "content": "Hi"}],
            "max_tokens": 1024,
        });

        let result = anthropic_to_openai(&body, "test-model");
        let messages = result["messages"].as_array().unwrap();
        assert_eq!(messages[0]["content"], "Part 1\n\nPart 2");
    }
}
