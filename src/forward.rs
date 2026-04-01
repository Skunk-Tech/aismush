use bytes::Bytes;
use http_body_util::{combinators::BoxBody, BodyExt, Full, StreamBody};
use hyper::body::Frame;
use hyper::{Request, Response, StatusCode};
use serde_json::Value;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Instant;
use tracing::{debug, error, info};

use crate::cost;
use crate::db;
use crate::memory;
use crate::state::ProxyState;
use crate::tokens;

fn full(data: impl Into<Bytes>) -> BoxBody<Bytes, hyper::Error> {
    Full::new(data.into()).map_err(|n| match n {}).boxed()
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

    match state.client.request(req).await {
        Ok(resp) => {
            let status = resp.status();
            let mut rb = Response::builder().status(status);
            for (k, v) in resp.headers() { rb = rb.header(k, v); }

            let body_stream = resp.into_body();
            let state2 = state.clone();
            let model = model.to_string();
            let reason = route_reason.to_string();
            let project = project_path.to_string();
            let (tx, rx) = tokio::sync::mpsc::channel::<Result<Frame<Bytes>, hyper::Error>>(64);

            tokio::spawn(async move {
                let mut total_bytes = 0u64;
                let mut stream_data = String::new();
                let mut stream = body_stream;
                loop {
                    match stream.frame().await {
                        Some(Ok(frame)) => {
                            if let Some(data) = frame.data_ref() {
                                total_bytes += data.len() as u64;
                                if let Ok(text) = std::str::from_utf8(data) {
                                    stream_data.push_str(text);
                                }
                            }
                            if tx.send(Ok(frame)).await.is_err() { break; }
                        }
                        Some(Err(e)) => { let _ = tx.send(Err(e)).await; break; }
                        None => break,
                    }
                }

                let usage = tokens::extract_usage(&stream_data);
                let latency = start.elapsed().as_millis() as u64;

                // Fallback token estimate if SSE parsing returned 0
                let input_tokens = if usage.input_tokens > 0 { usage.input_tokens } else { (total_bytes / 4) as u64 };
                let output_tokens = if usage.output_tokens > 0 { usage.output_tokens } else { (total_bytes / 8) as u64 };

                let costs = cost::calculate("claude", &model, input_tokens, output_tokens);

                {
                    let mut st = state2.stats.lock().await;
                    st.total_requests += 1;
                    st.claude_turns += 1;
                    st.claude_bytes += total_bytes;
                    st.estimated_tokens_routed += input_tokens + output_tokens;
                }

                if let Some(ref db) = state2.db {
                    db::log_request(
                        db, "claude", &model, &reason,
                        input_tokens, output_tokens,
                        usage.cache_read_tokens, usage.cache_write_tokens,
                        total_bytes, latency,
                        costs.actual_cost, costs.claude_equiv_cost,
                        comp_original, comp_final,
                    ).await;

                    // Extract memories from response
                    memory::extract_and_store(db, &project, &stream_data).await;
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
        Err(e) => {
            error!(error = %e, "Claude upstream error");
            Ok(json_resp(StatusCode::BAD_GATEWAY, &format!(r#"{{"error":"{}"}}"#, e)))
        }
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
            let (tx, rx) = tokio::sync::mpsc::channel::<Result<Frame<Bytes>, hyper::Error>>(64);

            tokio::spawn(async move {
                let mut total_bytes = 0u64;
                let mut stream_data = String::new();
                let mut stream = body_stream;
                loop {
                    match stream.frame().await {
                        Some(Ok(frame)) => {
                            if let Some(data) = frame.data_ref() {
                                total_bytes += data.len() as u64;
                                if let Ok(text) = std::str::from_utf8(data) {
                                    stream_data.push_str(text);
                                }
                            }
                            if tx.send(Ok(frame)).await.is_err() { break; }
                        }
                        Some(Err(e)) => { let _ = tx.send(Err(e)).await; break; }
                        None => break,
                    }
                }

                let usage = tokens::extract_usage(&stream_data);
                let latency = start.elapsed().as_millis() as u64;

                let input_tokens = if usage.input_tokens > 0 { usage.input_tokens } else { (total_bytes / 4) as u64 };
                let output_tokens = if usage.output_tokens > 0 { usage.output_tokens } else { (total_bytes / 8) as u64 };

                let costs = cost::calculate("deepseek", "deepseek-chat", input_tokens, output_tokens);

                {
                    let mut st = state2.stats.lock().await;
                    st.total_requests += 1;
                    st.deepseek_turns += 1;
                    st.deepseek_bytes += total_bytes;
                    st.estimated_tokens_routed += input_tokens + output_tokens;
                }

                if let Some(ref db) = state2.db {
                    db::log_request(
                        db, "deepseek", &model, &reason,
                        input_tokens, output_tokens,
                        usage.cache_read_tokens, usage.cache_write_tokens,
                        total_bytes, latency,
                        costs.actual_cost, costs.claude_equiv_cost,
                        comp_original, comp_final,
                    ).await;

                    memory::extract_and_store(db, &project, &stream_data).await;
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
