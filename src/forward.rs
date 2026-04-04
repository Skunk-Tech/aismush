use bytes::Bytes;
use http_body_util::{combinators::BoxBody, BodyExt, Full, StreamBody};
use hyper::body::Frame;
use hyper::{Request, Response, StatusCode};
use serde_json::Value;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Instant;
use tracing::{debug, error, warn};

use crate::capture;
use crate::cost;
use crate::db;
use crate::state::ProxyState;
use crate::tokens;
use crate::transform;

fn full(data: impl Into<Bytes>) -> BoxBody<Bytes, hyper::Error> {
    Full::new(data.into()).map_err(|n| match n {}).boxed()
}

/// Parse a single SSE event block, extracting usage and assistant text incrementally.
fn parse_sse_event(block: &str, usage: &mut tokens::TokenUsage, assistant_text: &mut String) {
    for line in block.lines() {
        let Some(json_str) = line.strip_prefix("data: ") else { continue };
        if !json_str.starts_with('{') { continue; }
        let Ok(val) = serde_json::from_str::<serde_json::Value>(json_str) else { continue };

        match val.get("type").and_then(|t| t.as_str()) {
            Some("message_start") => {
                if let Some(u) = val.get("message").and_then(|m| m.get("usage")) {
                    usage.input_tokens = u["input_tokens"].as_u64().unwrap_or(0);
                    usage.cache_read_tokens = u["cache_read_input_tokens"].as_u64().unwrap_or(0);
                    usage.cache_write_tokens = u["cache_creation_input_tokens"].as_u64().unwrap_or(0);
                }
            }
            Some("content_block_delta") => {
                if let Some(text) = val.get("delta").and_then(|d| d.get("text")).and_then(|t| t.as_str()) {
                    // Cap captured text at 32KB to prevent unbounded growth on large responses
                    if assistant_text.len() < 32_768 {
                        let remaining = 32_768 - assistant_text.len();
                        assistant_text.push_str(&text[..text.len().min(remaining)]);
                    }
                }
            }
            Some("message_delta") => {
                if let Some(u) = val.get("usage") {
                    usage.output_tokens = u["output_tokens"].as_u64().unwrap_or(0);
                }
            }
            _ => {}
        }
    }
}

pub fn json_resp(status: StatusCode, body: &str) -> Response<BoxBody<Bytes, hyper::Error>> {
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(full(Bytes::from(body.to_string())))
        .unwrap()
}

/// Forward request to Claude API.
pub async fn claude(
    parts: &hyper::http::request::Parts,
    body: &Bytes,
    path: &str,
    model: &str,
    route_reason: &str,
    project_path: &str,
    comp_original: u64,
    comp_final: u64,
    request_body: Option<&serde_json::Value>,
    state: &Arc<ProxyState>,
) -> Result<Response<BoxBody<Bytes, hyper::Error>>, Infallible> {
    let start = Instant::now();

    let mut builder = Request::builder()
        .method(parts.method.clone())
        .uri(format!("https://api.anthropic.com{}", path));

    for (k, v) in &parts.headers {
        if k == "host" || k == "connection" { continue; }
        builder = builder.header(k, v);
    }
    builder = builder.header("host", "api.anthropic.com");

    let req = match builder.body(full(body.clone())) {
        Ok(r) => r,
        Err(e) => {
            error!(error = %e, "Failed to build Claude request");
            return Ok(json_resp(StatusCode::INTERNAL_SERVER_ERROR, &format!(r#"{{"error":"{}"}}"#, e)));
        }
    };

    // Try Claude — if connection fails, fall back to DeepSeek (not a retry loop)
    let resp = match state.client.request(req).await {
        Ok(r) => r,
        Err(e) => {
            warn!(error = %e, "Claude connection failed");

            // Don't retry Claude — fall back to DeepSeek with trimmed context
            if !state.config.api_key.is_empty() {
                warn!("Falling back to DeepSeek (trimming context to fit)");

                // Build DeepSeek request from original body
                let mut ds_body: serde_json::Value = serde_json::from_slice(body).unwrap_or_default();
                ds_body["model"] = serde_json::Value::String("deepseek-chat".into());
                ds_body["temperature"] = serde_json::json!(0);
                if let serde_json::Value::Object(ref mut m) = ds_body {
                    m.remove("thinking");
                    m.remove("budget_tokens");
                }
                // Aggressively trim to fit DeepSeek window
                crate::context::ensure_fits(&mut ds_body, "deepseek");

                let ds_path = path.replace("/v1/", "/").replace("/v1", "/");
                let ds_bytes = serde_json::to_vec(&ds_body).unwrap_or_default();

                let ds_req = Request::builder()
                    .method(hyper::Method::POST)
                    .uri(format!("https://api.deepseek.com/anthropic{}", ds_path))
                    .header("host", "api.deepseek.com")
                    .header("content-type", "application/json")
                    .header("x-api-key", &state.config.api_key)
                    .header("authorization", format!("Bearer {}", &state.config.api_key))
                    .body(full(Bytes::from(ds_bytes)))
                    .unwrap();

                match state.client.request(ds_req).await {
                    Ok(r) => {
                        warn!("DeepSeek fallback succeeded");
                        r
                    }
                    Err(e2) => {
                        error!(error = %e2, "Both Claude and DeepSeek failed");
                        return Ok(json_resp(StatusCode::BAD_GATEWAY,
                            r#"{"error":"Both Claude and DeepSeek connections failed. Check your network."}"#));
                    }
                }
            } else {
                error!("Claude failed and no DeepSeek key configured");
                return Ok(json_resp(StatusCode::BAD_GATEWAY,
                    r#"{"error":"Claude connection failed. Configure a DeepSeek key for automatic fallback."}"#));
            }
        }
    };

    {
            let status = resp.status();
            let mut rb = Response::builder().status(status);
            for (k, v) in resp.headers() { rb = rb.header(k, v); }

            let body_stream = resp.into_body();
            let state2 = state.clone();
            let model = model.to_string();
            let reason = route_reason.to_string();
            let project = project_path.to_string();
            let user_msg = request_body.and_then(|b| capture::extract_user_message(b)).unwrap_or_default();
            let tool_calls = request_body.map(|b| capture::extract_tool_calls(b)).unwrap_or_default();
            let (tx, rx) = tokio::sync::mpsc::channel::<Result<Frame<Bytes>, hyper::Error>>(64);

            tokio::spawn(async move {
                let mut total_bytes = 0u64;
                let mut usage = tokens::TokenUsage::default();
                let mut assistant_text = String::with_capacity(4096);
                let mut sse_buf = String::with_capacity(16384);
                let mut stream = body_stream;
                loop {
                    match stream.frame().await {
                        Some(Ok(frame)) => {
                            if let Some(data) = frame.data_ref() {
                                total_bytes += data.len() as u64;
                                if let Ok(text) = std::str::from_utf8(data) {
                                    // Parse SSE events incrementally
                                    sse_buf.push_str(text);
                                    while let Some(pos) = sse_buf.find("\n\n") {
                                        let event_block = sse_buf[..pos].to_string();
                                        sse_buf.drain(..pos + 2);
                                        parse_sse_event(&event_block, &mut usage, &mut assistant_text);
                                    }
                                }
                            }
                            if tx.send(Ok(frame)).await.is_err() { break; }
                        }
                        Some(Err(e)) => { let _ = tx.send(Err(e)).await; break; }
                        None => break,
                    }
                }
                // Flush remaining SSE buffer
                if !sse_buf.is_empty() {
                    parse_sse_event(&sse_buf, &mut usage, &mut assistant_text);
                }

                let latency = start.elapsed().as_millis() as u64;

                // Fallback token estimate if SSE parsing returned 0
                let input_tokens = if usage.input_tokens > 0 { usage.input_tokens } else { (total_bytes / 4) as u64 };
                let output_tokens = if usage.output_tokens > 0 { usage.output_tokens } else { (total_bytes / 8) as u64 };

                // Tokens saved by compression (bytes_saved / 3 chars per token)
                let comp_tokens_saved = if comp_original > comp_final { (comp_original - comp_final) / 3 } else { 0 };
                let costs = cost::calculate_with_compression("claude", &model, input_tokens, output_tokens, comp_tokens_saved);

                {
                    let mut st = state2.stats.lock().await;
                    st.total_requests += 1;
                    st.claude_turns += 1;
                    st.claude_bytes += total_bytes;
                    st.estimated_tokens_routed += input_tokens + output_tokens;
                }

                if let Some(ref db) = state2.db {
                    // Compression savings: what these compressed-away tokens would have cost on Claude
                    let comp_savings = if comp_tokens_saved > 0 {
                        let (input_price, _) = crate::cost::get_pricing("claude", &model);
                        comp_tokens_saved as f64 * input_price / 1_000_000.0
                    } else { 0.0 };
                    db::log_request(
                        db, "claude", &model, &reason,
                        input_tokens, output_tokens,
                        usage.cache_read_tokens, usage.cache_write_tokens,
                        total_bytes, latency,
                        costs.actual_cost, costs.claude_equiv_cost,
                        comp_original, comp_final, comp_savings,
                    ).await;

                    // Capture full conversation turn
                    if !user_msg.is_empty() || !assistant_text.is_empty() {
                        let conv_id = capture::get_or_create_conversation(db, &project, false).await;
                        let embedder_guard = state2.embedder.read().await;
                        capture::store_turn(db, embedder_guard.as_ref(), conv_id, &capture::TurnData {
                            user_message: user_msg.clone(),
                            assistant_message: assistant_text,
                            tools: tool_calls.clone(),
                            provider: "claude".to_string(),
                            model: model.clone(),
                            route_reason: reason.clone(),
                            input_tokens, output_tokens,
                            latency_ms: latency,
                            cost: costs.actual_cost,
                        }).await;
                    }
                }

                debug!(
                    provider = "claude", model = %model,
                    input = input_tokens, output = output_tokens,
                    cost = format!("${:.4}", costs.actual_cost),
                    latency_ms = latency,
                    "Complete"
                );
            });

            Ok(rb.body(StreamBody::new(tokio_stream::wrappers::ReceiverStream::new(rx)).boxed()).unwrap())
    }
}

/// Forward request to DeepSeek API.
pub async fn deepseek(
    _body: &Bytes,
    path: &str,
    parsed: &Option<Value>,
    model: &str,
    route_reason: &str,
    project_path: &str,
    comp_original: u64,
    comp_final: u64,
    state: &Arc<ProxyState>,
) -> Result<Response<BoxBody<Bytes, hyper::Error>>, Infallible> {
    let start = Instant::now();

    if state.config.api_key.is_empty() {
        return Ok(json_resp(StatusCode::INTERNAL_SERVER_ERROR, r#"{"error":"no deepseek key"}"#));
    }

    let mut transformed = parsed.clone().unwrap_or(Value::Object(Default::default()));
    transformed["model"] = Value::String("deepseek-chat".into());
    transformed["temperature"] = serde_json::json!(0);
    if let Value::Object(ref mut m) = transformed {
        m.remove("thinking");
        m.remove("budget_tokens");
    }
    if transformed["max_tokens"].as_u64().unwrap_or(0) > 16384 {
        transformed["max_tokens"] = serde_json::json!(16384);
    }

    let ds_path = path.replace("/v1/", "/").replace("/v1", "/");
    let uri = format!("https://api.deepseek.com/anthropic{}", ds_path);
    let body_bytes = serde_json::to_vec(&transformed).unwrap_or_default();

    let req = Request::builder()
        .method(hyper::Method::POST)
        .uri(&uri)
        .header("host", "api.deepseek.com")
        .header("content-type", "application/json")
        .header("x-api-key", &state.config.api_key)
        .header("authorization", format!("Bearer {}", &state.config.api_key))
        .body(full(Bytes::from(body_bytes)))
        .unwrap();

    match state.client.request(req).await {
        Ok(resp) => {
            let status = resp.status();

            // If DeepSeek returns server error, return clear error message
            if status.as_u16() >= 500 {
                let err_body = resp.into_body().collect().await
                    .map(|b| String::from_utf8_lossy(&b.to_bytes()).to_string())
                    .unwrap_or_default();
                error!(status = status.as_u16(), "DeepSeek server error, consider FORCE_PROVIDER=claude");
                return Ok(json_resp(status, &format!(
                    r#"{{"error":"DeepSeek returned {}. The proxy will retry on Claude for future requests if errors persist.","details":{}}}"#,
                    status, serde_json::to_string(&err_body).unwrap_or("\"\"".into())
                )));
            }

            let mut rb = Response::builder().status(status);
            for (k, v) in resp.headers() { rb = rb.header(k, v); }

            let body_stream = resp.into_body();
            let state2 = state.clone();
            let model = model.to_string();
            let reason = route_reason.to_string();
            let project = project_path.to_string();
            let ds_user_msg = parsed.as_ref().and_then(|b| capture::extract_user_message(b)).unwrap_or_default();
            let ds_tool_calls = parsed.as_ref().map(|b| capture::extract_tool_calls(b)).unwrap_or_default();
            let (tx, rx) = tokio::sync::mpsc::channel::<Result<Frame<Bytes>, hyper::Error>>(64);

            tokio::spawn(async move {
                let mut total_bytes = 0u64;
                let mut usage = tokens::TokenUsage::default();
                let mut assistant_text = String::with_capacity(4096);
                let mut sse_buf = String::with_capacity(16384);
                let mut stream = body_stream;
                loop {
                    match stream.frame().await {
                        Some(Ok(frame)) => {
                            if let Some(data) = frame.data_ref() {
                                total_bytes += data.len() as u64;
                                if let Ok(text) = std::str::from_utf8(data) {
                                    sse_buf.push_str(text);
                                    while let Some(pos) = sse_buf.find("\n\n") {
                                        let event_block = sse_buf[..pos].to_string();
                                        sse_buf.drain(..pos + 2);
                                        parse_sse_event(&event_block, &mut usage, &mut assistant_text);
                                    }
                                }
                            }
                            if tx.send(Ok(frame)).await.is_err() { break; }
                        }
                        Some(Err(e)) => { let _ = tx.send(Err(e)).await; break; }
                        None => break,
                    }
                }
                if !sse_buf.is_empty() {
                    parse_sse_event(&sse_buf, &mut usage, &mut assistant_text);
                }

                let latency = start.elapsed().as_millis() as u64;

                let input_tokens = if usage.input_tokens > 0 { usage.input_tokens } else { (total_bytes / 4) as u64 };
                let output_tokens = if usage.output_tokens > 0 { usage.output_tokens } else { (total_bytes / 8) as u64 };

                let comp_tokens_saved = if comp_original > comp_final { (comp_original - comp_final) / 3 } else { 0 };
                let costs = cost::calculate_with_compression("deepseek", "deepseek-chat", input_tokens, output_tokens, comp_tokens_saved);

                {
                    let mut st = state2.stats.lock().await;
                    st.total_requests += 1;
                    st.deepseek_turns += 1;
                    st.deepseek_bytes += total_bytes;
                    st.estimated_tokens_routed += input_tokens + output_tokens;
                }

                if let Some(ref db) = state2.db {
                    let comp_savings = if comp_tokens_saved > 0 {
                        let (input_price, _) = crate::cost::get_pricing("claude", "claude-sonnet-4-20250514");
                        comp_tokens_saved as f64 * input_price / 1_000_000.0
                    } else { 0.0 };
                    db::log_request(
                        db, "deepseek", &model, &reason,
                        input_tokens, output_tokens,
                        usage.cache_read_tokens, usage.cache_write_tokens,
                        total_bytes, latency,
                        costs.actual_cost, costs.claude_equiv_cost,
                        comp_original, comp_final, comp_savings,
                    ).await;

                    if !ds_user_msg.is_empty() || !assistant_text.is_empty() {
                        let conv_id = capture::get_or_create_conversation(db, &project, false).await;
                        let embedder_guard = state2.embedder.read().await;
                        capture::store_turn(db, embedder_guard.as_ref(), conv_id, &capture::TurnData {
                            user_message: ds_user_msg.clone(),
                            assistant_message: assistant_text,
                            tools: ds_tool_calls.clone(),
                            provider: "deepseek".to_string(),
                            model: model.clone(),
                            route_reason: reason.clone(),
                            input_tokens, output_tokens,
                            latency_ms: latency,
                            cost: costs.actual_cost,
                        }).await;
                    }
                }

                debug!(
                    provider = "deepseek",
                    input = input_tokens, output = output_tokens,
                    cost = format!("${:.4}", costs.actual_cost),
                    saved = format!("${:.4}", costs.savings),
                    latency_ms = latency,
                    "Complete"
                );
            });

            Ok(rb.body(StreamBody::new(tokio_stream::wrappers::ReceiverStream::new(rx)).boxed()).unwrap())
        }
        Err(e) => {
            error!(error = %e, "DeepSeek upstream error");
            Ok(json_resp(StatusCode::BAD_GATEWAY, &format!(r#"{{"error":"{}"}}"#, e)))
        }
    }
}

/// Forward request to any OpenAI-compatible provider (OpenRouter, Ollama, LM Studio, etc.).
///
/// Converts the Anthropic-format request to OpenAI format, streams the response,
/// and converts the OpenAI SSE back to Anthropic SSE for Claude Code.
pub async fn openai_compat(
    parsed: &Option<Value>,
    provider_id: &str,
    base_url: &str,
    api_key: Option<&str>,
    target_model: &str,
    route_reason: &str,
    project_path: &str,
    comp_original: u64,
    comp_final: u64,
    state: &Arc<ProxyState>,
) -> Result<Response<BoxBody<Bytes, hyper::Error>>, Infallible> {
    let start = Instant::now();

    let body = parsed.clone().unwrap_or(Value::Object(Default::default()));

    // Convert Anthropic request format to OpenAI format
    let openai_body = transform::anthropic_to_openai(&body, target_model);
    let body_bytes = serde_json::to_vec(&openai_body).unwrap_or_default();

    // Build the URL — ensure we hit /v1/chat/completions
    let url = if base_url.contains("/v1") {
        format!("{}/chat/completions", base_url.trim_end_matches('/'))
    } else {
        format!("{}/v1/chat/completions", base_url.trim_end_matches('/'))
    };

    let mut builder = Request::builder()
        .method(hyper::Method::POST)
        .uri(&url)
        .header("content-type", "application/json");

    // Auth header (not needed for most local servers)
    if let Some(key) = api_key {
        builder = builder.header("authorization", format!("Bearer {}", key));
    }

    // OpenRouter-specific headers
    if provider_id == "openrouter" || base_url.contains("openrouter.ai") {
        builder = builder.header("HTTP-Referer", "https://aismush.us.com");
        builder = builder.header("X-Title", "AISmush");
    }

    // Extract host from URL for host header
    if let Some(host) = url.split("://").nth(1).and_then(|s| s.split('/').next()).and_then(|s| s.split(':').next()) {
        builder = builder.header("host", host);
    }

    let req = match builder.body(full(Bytes::from(body_bytes))) {
        Ok(r) => r,
        Err(e) => {
            error!(error = %e, provider = provider_id, "Failed to build request");
            return Ok(json_resp(StatusCode::INTERNAL_SERVER_ERROR, &format!(r#"{{"error":"{}"}}"#, e)));
        }
    };

    match state.client.request(req).await {
        Ok(resp) => {
            let status = resp.status();

            if status.as_u16() >= 500 {
                let err_body = resp.into_body().collect().await
                    .map(|b| String::from_utf8_lossy(&b.to_bytes()).to_string())
                    .unwrap_or_default();
                error!(status = status.as_u16(), provider = provider_id, "Provider server error");
                return Ok(json_resp(StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY), &format!(
                    r#"{{"error":"{} returned {}","details":{}}}"#,
                    provider_id, status, serde_json::to_string(&err_body).unwrap_or("\"\"".into())
                )));
            }

            // Stream response, converting OpenAI SSE -> Anthropic SSE
            let body_stream = resp.into_body();
            let state2 = state.clone();
            let provider_id = provider_id.to_string();
            let model = target_model.to_string();
            let reason = route_reason.to_string();
            let project = project_path.to_string();
            let user_msg = parsed.as_ref().and_then(|b| capture::extract_user_message(b)).unwrap_or_default();
            let tool_calls = parsed.as_ref().map(|b| capture::extract_tool_calls(b)).unwrap_or_default();
            let (tx, rx) = tokio::sync::mpsc::channel::<Result<Frame<Bytes>, hyper::Error>>(64);

            tokio::spawn(async move {
                let mut total_bytes = 0u64;
                let mut assistant_text = String::with_capacity(4096);
                let mut sse_buf = String::new();
                let mut converter = transform::OpenAIToAnthropicStream::new(&model);
                let mut stream = body_stream;

                loop {
                    match stream.frame().await {
                        Some(Ok(frame)) => {
                            if let Some(data) = frame.data_ref() {
                                total_bytes += data.len() as u64;
                                if let Ok(text) = std::str::from_utf8(data) {
                                    sse_buf.push_str(text);

                                    // Process complete SSE lines
                                    while let Some(pos) = sse_buf.find('\n') {
                                        let line = sse_buf[..pos].trim().to_string();
                                        sse_buf.drain(..pos + 1);

                                        if line.is_empty() { continue; }

                                        let data_str = if let Some(d) = line.strip_prefix("data: ") {
                                            d
                                        } else {
                                            continue;
                                        };

                                        // Extract assistant text from OpenAI chunk BEFORE conversion
                                        // (much more reliable than parsing from converted SSE events)
                                        if data_str != "[DONE]" {
                                            if let Ok(chunk) = serde_json::from_str::<serde_json::Value>(data_str) {
                                                if let Some(choices) = chunk.get("choices").and_then(|c| c.as_array()) {
                                                    for choice in choices {
                                                        if let Some(text) = choice.get("delta").and_then(|d| d.get("content")).and_then(|c| c.as_str()) {
                                                            if assistant_text.len() < 32_768 {
                                                                let remaining = 32_768 - assistant_text.len();
                                                                assistant_text.push_str(&text[..text.len().min(remaining)]);
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }

                                        // Convert OpenAI SSE -> Anthropic SSE
                                        let anthropic_events = converter.process_chunk(data_str);
                                        for event in &anthropic_events {
                                            let event_bytes = Bytes::from(event.clone());
                                            if tx.send(Ok(Frame::data(event_bytes))).await.is_err() {
                                                return;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        Some(Err(e)) => { let _ = tx.send(Err(e)).await; break; }
                        None => break,
                    }
                }

                // Flush remaining buffer
                if !sse_buf.is_empty() {
                    let line = sse_buf.trim().to_string();
                    if let Some(data_str) = line.strip_prefix("data: ") {
                        let events = converter.process_chunk(data_str);
                        for event in events {
                            let _ = tx.send(Ok(Frame::data(Bytes::from(event)))).await;
                        }
                    }
                }

                let latency = start.elapsed().as_millis() as u64;
                let input_tokens = if converter.input_tokens() > 0 { converter.input_tokens() } else { (total_bytes / 4) as u64 };
                let output_tokens = if converter.output_tokens() > 0 { converter.output_tokens() } else { (total_bytes / 8) as u64 };

                let comp_tokens_saved = if comp_original > comp_final { (comp_original - comp_final) / 3 } else { 0 };
                let costs = cost::calculate_with_compression(&provider_id, &model, input_tokens, output_tokens, comp_tokens_saved);

                {
                    let mut st = state2.stats.lock().await;
                    st.total_requests += 1;
                    st.estimated_tokens_routed += input_tokens + output_tokens;
                    // Update dynamic provider counters
                    *st.provider_turns.entry(provider_id.clone()).or_insert(0) += 1;
                    *st.provider_bytes.entry(provider_id.clone()).or_insert(0) += total_bytes;
                    // Also update legacy counters for backward compat
                    if provider_id == "deepseek" {
                        st.deepseek_turns += 1;
                        st.deepseek_bytes += total_bytes;
                    }
                }

                if let Some(ref db) = state2.db {
                    let comp_savings = if comp_original > comp_final {
                        let tokens_saved = (comp_original - comp_final) / 3;
                        let (input_price, _) = crate::cost::get_pricing("claude", "claude-sonnet-4-20250514");
                        tokens_saved as f64 * input_price / 1_000_000.0
                    } else { 0.0 };
                    db::log_request(
                        db, &provider_id, &model, &reason,
                        input_tokens, output_tokens,
                        0, 0, // no cache tokens for OpenAI-compat providers
                        total_bytes, latency,
                        costs.actual_cost, costs.claude_equiv_cost,
                        comp_original, comp_final, comp_savings,
                    ).await;

                    if !user_msg.is_empty() || !assistant_text.is_empty() {
                        let conv_id = capture::get_or_create_conversation(db, &project, false).await;
                        let embedder_guard = state2.embedder.read().await;
                        capture::store_turn(db, embedder_guard.as_ref(), conv_id, &capture::TurnData {
                            user_message: user_msg.clone(),
                            assistant_message: assistant_text,
                            tools: tool_calls.clone(),
                            provider: provider_id.clone(),
                            model: model.clone(),
                            route_reason: reason.clone(),
                            input_tokens, output_tokens,
                            latency_ms: latency,
                            cost: costs.actual_cost,
                        }).await;
                    }
                }

                debug!(
                    provider = %provider_id, model = %model,
                    input = input_tokens, output = output_tokens,
                    cost = format!("${:.4}", costs.actual_cost),
                    saved = format!("${:.4}", costs.savings),
                    latency_ms = latency,
                    "Complete (OpenAI-compat)"
                );
            });

            // Return response with Anthropic-compatible headers
            let rb = Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "text/event-stream")
                .header("cache-control", "no-cache");

            Ok(rb.body(StreamBody::new(tokio_stream::wrappers::ReceiverStream::new(rx)).boxed()).unwrap())
        }
        Err(e) => {
            error!(error = %e, provider = provider_id, "Upstream connection error");
            Ok(json_resp(StatusCode::BAD_GATEWAY, &format!(r#"{{"error":"{} connection failed: {}"}}"#, provider_id, e)))
        }
    }
}
