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

// ── OpenAI → Anthropic request conversion (reverse of anthropic_to_openai) ──

/// Convert an OpenAI /v1/chat/completions request body to Anthropic Messages API format.
pub fn openai_to_anthropic(body: &Value) -> Value {
    let mut system: Option<Value> = None;
    let mut messages: Vec<Value> = Vec::new();

    if let Some(msgs) = body.get("messages").and_then(|m| m.as_array()) {
        // Accumulator for consecutive tool-role messages → grouped into one user message
        let mut pending_tool_results: Vec<Value> = Vec::new();

        for msg in msgs {
            let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("user");

            match role {
                "system" => {
                    // OpenAI system message → Anthropic top-level system field
                    let text = msg.get("content").and_then(|c| c.as_str()).unwrap_or("");
                    system = Some(json!(text));
                }
                "tool" => {
                    // OpenAI tool result → accumulate as Anthropic tool_result block
                    let tool_call_id = msg.get("tool_call_id").and_then(|i| i.as_str()).unwrap_or("");
                    let content = msg.get("content").and_then(|c| c.as_str()).unwrap_or("");
                    pending_tool_results.push(json!({
                        "type": "tool_result",
                        "tool_use_id": tool_call_id,
                        "content": content,
                    }));
                }
                _ => {
                    // Flush any pending tool results as a user message
                    if !pending_tool_results.is_empty() {
                        messages.push(json!({
                            "role": "user",
                            "content": pending_tool_results.drain(..).collect::<Vec<_>>(),
                        }));
                    }

                    if role == "assistant" {
                        let mut content_blocks: Vec<Value> = Vec::new();

                        // Text content
                        if let Some(text) = msg.get("content").and_then(|c| c.as_str()) {
                            if !text.is_empty() {
                                content_blocks.push(json!({"type": "text", "text": text}));
                            }
                        }

                        // Tool calls → tool_use blocks
                        if let Some(tool_calls) = msg.get("tool_calls").and_then(|t| t.as_array()) {
                            for tc in tool_calls {
                                let id = tc.get("id").and_then(|i| i.as_str()).unwrap_or("");
                                if let Some(func) = tc.get("function") {
                                    let name = func.get("name").and_then(|n| n.as_str()).unwrap_or("");
                                    let args_str = func.get("arguments").and_then(|a| a.as_str()).unwrap_or("{}");
                                    let input: Value = serde_json::from_str(args_str).unwrap_or(json!({}));
                                    content_blocks.push(json!({
                                        "type": "tool_use",
                                        "id": id,
                                        "name": name,
                                        "input": input,
                                    }));
                                }
                            }
                        }

                        if content_blocks.is_empty() {
                            content_blocks.push(json!({"type": "text", "text": ""}));
                        }
                        messages.push(json!({"role": "assistant", "content": content_blocks}));
                    } else {
                        // User message — convert string content to block array
                        let content = msg.get("content").and_then(|c| c.as_str()).unwrap_or("");
                        messages.push(json!({
                            "role": "user",
                            "content": [{"type": "text", "text": content}],
                        }));
                    }
                }
            }
        }

        // Flush remaining tool results
        if !pending_tool_results.is_empty() {
            messages.push(json!({
                "role": "user",
                "content": pending_tool_results,
            }));
        }
    }

    let mut result = json!({
        "messages": messages,
        "stream": true,
    });

    // Copy model
    if let Some(model) = body.get("model").and_then(|m| m.as_str()) {
        result["model"] = json!(model);
    }

    // Set system
    if let Some(sys) = system {
        result["system"] = sys;
    }

    // Copy max_tokens
    if let Some(max) = body.get("max_tokens").and_then(|m| m.as_u64()) {
        result["max_tokens"] = json!(max);
    }

    // Copy temperature
    if let Some(temp) = body.get("temperature").and_then(|t| t.as_f64()) {
        result["temperature"] = json!(temp);
    }

    // Convert OpenAI tools to Anthropic format
    if let Some(tools) = body.get("tools").and_then(|t| t.as_array()) {
        let anthropic_tools: Vec<Value> = tools.iter().filter_map(|tool| {
            let func = tool.get("function")?;
            Some(json!({
                "name": func.get("name").and_then(|n| n.as_str()).unwrap_or(""),
                "description": func.get("description").and_then(|d| d.as_str()).unwrap_or(""),
                "input_schema": func.get("parameters").cloned().unwrap_or(json!({})),
            }))
        }).collect();
        if !anthropic_tools.is_empty() {
            result["tools"] = json!(anthropic_tools);
        }
    }

    result
}

// ── Anthropic SSE → OpenAI SSE stream conversion ──

/// State machine for converting Anthropic SSE stream to OpenAI SSE format.
/// Used when the inbound client expects OpenAI format responses.
pub struct AnthropicToOpenAIStream {
    model: String,
    response_id: String,
    tool_call_index: i32,
    input_tokens: u64,
    output_tokens: u64,
}

impl AnthropicToOpenAIStream {
    pub fn new(model: &str) -> Self {
        Self {
            model: model.to_string(),
            response_id: format!("chatcmpl-{:016x}", rand_id()),
            tool_call_index: -1,
            input_tokens: 0,
            output_tokens: 0,
        }
    }

    /// Process a single Anthropic SSE event and return zero or more OpenAI SSE lines.
    pub fn process_event(&mut self, event_type: &str, data: &str) -> Vec<String> {
        let mut lines = Vec::new();

        let Ok(parsed) = serde_json::from_str::<Value>(data) else {
            return lines;
        };

        match event_type {
            "message_start" => {
                // Extract usage from message_start
                if let Some(usage) = parsed.get("message").and_then(|m| m.get("usage")) {
                    if let Some(it) = usage.get("input_tokens").and_then(|t| t.as_u64()) {
                        self.input_tokens = it;
                    }
                }
                // Emit first chunk with role
                lines.push(self.openai_chunk(json!({
                    "role": "assistant",
                    "content": "",
                }), Value::Null));
            }
            "content_block_start" => {
                if let Some(cb) = parsed.get("content_block") {
                    if cb.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                        self.tool_call_index += 1;
                        let id = cb.get("id").and_then(|i| i.as_str()).unwrap_or("");
                        let name = cb.get("name").and_then(|n| n.as_str()).unwrap_or("");
                        lines.push(self.openai_chunk(json!({
                            "tool_calls": [{
                                "index": self.tool_call_index,
                                "id": id,
                                "type": "function",
                                "function": {"name": name, "arguments": ""},
                            }]
                        }), Value::Null));
                    }
                }
            }
            "content_block_delta" => {
                if let Some(delta) = parsed.get("delta") {
                    match delta.get("type").and_then(|t| t.as_str()) {
                        Some("text_delta") => {
                            let text = delta.get("text").and_then(|t| t.as_str()).unwrap_or("");
                            if !text.is_empty() {
                                lines.push(self.openai_chunk(json!({"content": text}), Value::Null));
                            }
                        }
                        Some("input_json_delta") => {
                            let partial = delta.get("partial_json").and_then(|p| p.as_str()).unwrap_or("");
                            if !partial.is_empty() && self.tool_call_index >= 0 {
                                lines.push(self.openai_chunk(json!({
                                    "tool_calls": [{
                                        "index": self.tool_call_index,
                                        "function": {"arguments": partial},
                                    }]
                                }), Value::Null));
                            }
                        }
                        _ => {}
                    }
                }
            }
            "content_block_stop" => {
                // No direct OpenAI equivalent needed
            }
            "message_delta" => {
                if let Some(usage) = parsed.get("usage") {
                    if let Some(ot) = usage.get("output_tokens").and_then(|t| t.as_u64()) {
                        self.output_tokens = ot;
                    }
                }
                let stop_reason = parsed.get("delta")
                    .and_then(|d| d.get("stop_reason"))
                    .and_then(|r| r.as_str())
                    .unwrap_or("stop");
                let finish_reason = match stop_reason {
                    "end_turn" => "stop",
                    "tool_use" => "tool_calls",
                    "max_tokens" => "length",
                    other => other,
                };
                lines.push(self.openai_chunk_with_finish(finish_reason));
            }
            "message_stop" => {
                lines.push("data: [DONE]\n\n".to_string());
            }
            _ => {}
        }

        lines
    }

    fn openai_chunk(&self, delta: Value, finish_reason: Value) -> String {
        let chunk = json!({
            "id": self.response_id,
            "object": "chat.completion.chunk",
            "model": self.model,
            "choices": [{
                "index": 0,
                "delta": delta,
                "finish_reason": finish_reason,
            }]
        });
        format!("data: {}\n\n", chunk)
    }

    fn openai_chunk_with_finish(&self, reason: &str) -> String {
        let chunk = json!({
            "id": self.response_id,
            "object": "chat.completion.chunk",
            "model": self.model,
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": reason,
            }],
            "usage": {
                "prompt_tokens": self.input_tokens,
                "completion_tokens": self.output_tokens,
                "total_tokens": self.input_tokens + self.output_tokens,
            }
        });
        format!("data: {}\n\n", chunk)
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

    // ── OpenAI → Anthropic conversion tests ──

    #[test]
    fn test_openai_to_anthropic_simple() {
        let body = json!({
            "model": "gpt-4",
            "messages": [
                {"role": "system", "content": "You are helpful."},
                {"role": "user", "content": "Hello"},
            ],
            "max_tokens": 1024,
        });

        let result = openai_to_anthropic(&body);
        assert_eq!(result["system"], "You are helpful.");
        let messages = result["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"][0]["type"], "text");
        assert_eq!(messages[0]["content"][0]["text"], "Hello");
        assert_eq!(result["max_tokens"], 1024);
    }

    #[test]
    fn test_openai_to_anthropic_tool_calls() {
        let body = json!({
            "model": "gpt-4",
            "messages": [
                {"role": "user", "content": "Read main.rs"},
                {"role": "assistant", "content": null, "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {"name": "read_file", "arguments": "{\"path\":\"/src/main.rs\"}"}
                }]},
                {"role": "tool", "tool_call_id": "call_1", "content": "fn main() {}"},
            ],
        });

        let result = openai_to_anthropic(&body);
        let messages = result["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 3);

        // Assistant has tool_use block
        assert_eq!(messages[1]["role"], "assistant");
        let content = messages[1]["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "tool_use");
        assert_eq!(content[0]["name"], "read_file");
        assert_eq!(content[0]["input"]["path"], "/src/main.rs");

        // Tool result grouped into user message
        assert_eq!(messages[2]["role"], "user");
        let tool_results = messages[2]["content"].as_array().unwrap();
        assert_eq!(tool_results[0]["type"], "tool_result");
        assert_eq!(tool_results[0]["tool_use_id"], "call_1");
        assert_eq!(tool_results[0]["content"], "fn main() {}");
    }

    #[test]
    fn test_openai_to_anthropic_tools_definition() {
        let body = json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "test"}],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "get_weather",
                    "description": "Get weather",
                    "parameters": {"type": "object", "properties": {"city": {"type": "string"}}},
                }
            }],
        });

        let result = openai_to_anthropic(&body);
        let tools = result["tools"].as_array().unwrap();
        assert_eq!(tools[0]["name"], "get_weather");
        assert_eq!(tools[0]["input_schema"]["type"], "object");
    }

    #[test]
    fn test_openai_to_anthropic_multiple_tool_results() {
        let body = json!({
            "model": "gpt-4",
            "messages": [
                {"role": "user", "content": "Read two files"},
                {"role": "assistant", "content": null, "tool_calls": [
                    {"id": "c1", "type": "function", "function": {"name": "read", "arguments": "{}"}},
                    {"id": "c2", "type": "function", "function": {"name": "read", "arguments": "{}"}},
                ]},
                {"role": "tool", "tool_call_id": "c1", "content": "file1"},
                {"role": "tool", "tool_call_id": "c2", "content": "file2"},
            ],
        });

        let result = openai_to_anthropic(&body);
        let messages = result["messages"].as_array().unwrap();
        // Two consecutive tool messages → one user message with two tool_result blocks
        let last = &messages[messages.len() - 1];
        assert_eq!(last["role"], "user");
        let results = last["content"].as_array().unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0]["tool_use_id"], "c1");
        assert_eq!(results[1]["tool_use_id"], "c2");
    }

    // ── Anthropic → OpenAI SSE conversion tests ──

    #[test]
    fn test_anthropic_to_openai_stream_text() {
        let mut stream = AnthropicToOpenAIStream::new("claude-sonnet");

        let events = stream.process_event("message_start", &json!({
            "type": "message_start",
            "message": {"usage": {"input_tokens": 100}}
        }).to_string());
        assert!(events.iter().any(|e| e.contains("chat.completion.chunk")));
        assert!(events.iter().any(|e| e.contains("\"role\":\"assistant\"")));

        let events = stream.process_event("content_block_delta", &json!({
            "type": "content_block_delta",
            "delta": {"type": "text_delta", "text": "Hello"}
        }).to_string());
        assert!(events.iter().any(|e| e.contains("\"content\":\"Hello\"")));

        let events = stream.process_event("message_delta", &json!({
            "type": "message_delta",
            "delta": {"stop_reason": "end_turn"},
            "usage": {"output_tokens": 5}
        }).to_string());
        assert!(events.iter().any(|e| e.contains("\"finish_reason\":\"stop\"")));

        let events = stream.process_event("message_stop", &json!({"type": "message_stop"}).to_string());
        assert!(events.iter().any(|e| e.contains("[DONE]")));
    }

    #[test]
    fn test_anthropic_to_openai_stream_tool_use() {
        let mut stream = AnthropicToOpenAIStream::new("claude-sonnet");

        stream.process_event("message_start", &json!({
            "type": "message_start",
            "message": {"usage": {"input_tokens": 50}}
        }).to_string());

        let events = stream.process_event("content_block_start", &json!({
            "type": "content_block_start",
            "content_block": {"type": "tool_use", "id": "call_1", "name": "read_file", "input": {}}
        }).to_string());
        assert!(events.iter().any(|e| e.contains("tool_calls") && e.contains("read_file")));

        let events = stream.process_event("content_block_delta", &json!({
            "type": "content_block_delta",
            "delta": {"type": "input_json_delta", "partial_json": "{\"path\":"}
        }).to_string());
        assert!(events.iter().any(|e| e.contains("arguments") && e.contains("{\\\"path\\\":")));

        let events = stream.process_event("message_delta", &json!({
            "type": "message_delta",
            "delta": {"stop_reason": "tool_use"},
            "usage": {"output_tokens": 20}
        }).to_string());
        assert!(events.iter().any(|e| e.contains("\"finish_reason\":\"tool_calls\"")));
    }

    // ── Round-trip test ──

    #[test]
    fn test_round_trip_anthropic_openai_anthropic() {
        let original = json!({
            "model": "claude-sonnet-4-20250514",
            "system": "You are helpful.",
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": "Hello"}]},
                {"role": "assistant", "content": [
                    {"type": "text", "text": "I'll read that."},
                    {"type": "tool_use", "id": "t1", "name": "Read", "input": {"file_path": "/main.rs"}},
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "t1", "content": "fn main() {}"},
                ]},
            ],
            "max_tokens": 4096,
        });

        // Anthropic → OpenAI
        let openai = anthropic_to_openai(&original, "gpt-4");
        // OpenAI → Anthropic
        let back = openai_to_anthropic(&openai);

        // Verify key structure preserved
        assert_eq!(back["system"], "You are helpful.");
        let msgs = back["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(msgs[1]["role"], "assistant");
        assert_eq!(msgs[2]["role"], "user");

        // Tool use preserved
        let assistant_content = msgs[1]["content"].as_array().unwrap();
        assert!(assistant_content.iter().any(|b| b["type"] == "tool_use" && b["name"] == "Read"));

        // Tool result preserved
        let tool_content = msgs[2]["content"].as_array().unwrap();
        assert!(tool_content.iter().any(|b| b["type"] == "tool_result" && b["tool_use_id"] == "t1"));
    }
}
