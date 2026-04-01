mod compress;
mod config;
mod context;
mod cost;
mod dashboard;
mod db;
mod forward;
mod memory;
mod router;
mod state;
mod tokens;

use bytes::Bytes;
use http_body_util::{combinators::BoxBody, BodyExt, Full};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_rustls::HttpsConnectorBuilder;
use hyper_util::client::legacy::Client;
use hyper_util::rt::{TokioExecutor, TokioIo};
use std::convert::Infallible;
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing::{debug, error, info, warn};

use crate::state::ProxyState;

fn full(data: impl Into<Bytes>) -> BoxBody<Bytes, hyper::Error> {
    Full::new(data.into()).map_err(|n| match n {}).boxed()
}

fn json_resp(status: StatusCode, body: &str) -> Response<BoxBody<Bytes, hyper::Error>> {
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(full(Bytes::from(body.to_string())))
        .unwrap()
}

#[tokio::main]
async fn main() {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");

    // Load config
    let cfg = config::ProxyConfig::load();

    // Init logging
    let filter = if cfg.verbose { "debug" } else { "info" };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(filter)),
        )
        .with_target(false)
        .compact()
        .init();

    if cfg.api_key.is_empty() {
        warn!("No DeepSeek API key configured");
    } else {
        info!(key = &cfg.api_key[..8.min(cfg.api_key.len())], "DeepSeek key loaded");
    }

    if let Some(ref fp) = cfg.force_provider {
        info!(provider = %fp, "Forced provider");
    } else {
        info!("Hybrid routing mode");
    }

    info!(path = %cfg.data_dir.display(), "Data directory");

    // Open database
    let database = match db::open(&cfg.db_path) {
        Ok(db) => Some(db),
        Err(e) => {
            warn!(error = %e, "Failed to open database, running without persistence");
            None
        }
    };

    // Build HTTPS client
    let https = HttpsConnectorBuilder::new()
        .with_webpki_roots()
        .https_only()
        .enable_http1()
        .build();

    let client = Client::builder(TokioExecutor::new())
        .pool_idle_timeout(std::time::Duration::from_secs(90))
        .pool_max_idle_per_host(10)
        .build(https);

    // Decay old memory scores on startup
    if let Some(ref db) = database {
        memory::decay_scores(db).await;
    }

    let state = ProxyState::new(cfg.clone(), client, database);

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], cfg.port));
    let listener = TcpListener::bind(addr).await.expect("Failed to bind port");
    info!(port = cfg.port, "Listening");
    info!("Dashboard: http://127.0.0.1:{}/dashboard", cfg.port);

    // Graceful shutdown
    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, _)) => {
                        let state = state.clone();
                        tokio::spawn(async move {
                            let io = TokioIo::new(stream);
                            let _ = http1::Builder::new()
                                .preserve_header_case(true)
                                .serve_connection(io, service_fn(move |req| {
                                    let s = state.clone();
                                    async move { handle(req, s).await }
                                }))
                                .with_upgrades()
                                .await;
                        });
                    }
                    Err(e) => warn!(error = %e, "Accept error"),
                }
            }
            _ = &mut shutdown => {
                info!("Shutting down...");
                break;
            }
        }
    }
}

async fn handle(
    req: Request<hyper::body::Incoming>,
    state: Arc<ProxyState>,
) -> Result<Response<BoxBody<Bytes, hyper::Error>>, Infallible> {
    let path = req.uri().path_and_query().map(|pq| pq.as_str().to_string()).unwrap_or("/".into());
    let method = req.method().clone();

    // ── Static endpoints ────────────────────────────────────────────────
    if method == hyper::Method::GET {
        if path == "/health" {
            return Ok(json_resp(StatusCode::OK, r#"{"status":"ok"}"#));
        }
        if path == "/stats" {
            if let Some(ref database) = state.db {
                let stats = db::get_stats(database).await;
                return Ok(json_resp(StatusCode::OK, &stats.to_string()));
            } else {
                let s = state.stats.lock().await;
                return Ok(json_resp(StatusCode::OK, &serde_json::to_string(&*s).unwrap()));
            }
        }
        if path == "/history" {
            if let Some(ref database) = state.db {
                let rows = db::recent_requests(database, 20).await;
                return Ok(json_resp(StatusCode::OK, &serde_json::to_string(&rows).unwrap()));
            }
            return Ok(json_resp(StatusCode::OK, "[]"));
        }
        if path.starts_with("/dashboard") {
            let html = dashboard::render(&state.db, state.config.port).await;
            return Ok(Response::builder().status(200).header("content-type", "text/html").body(full(html)).unwrap());
        }
        if path.starts_with("/memories") {
            if let Some(ref database) = state.db {
                let memories = db::get_memories(database).await;
                return Ok(json_resp(StatusCode::OK, &serde_json::to_string(&memories).unwrap()));
            }
            return Ok(json_resp(StatusCode::OK, "[]"));
        }
    }
    if method == hyper::Method::POST && path == "/memories/clear" {
        if let Some(ref database) = state.db {
            db::clear_memories(database).await;
            return Ok(json_resp(StatusCode::OK, r#"{"ok":true}"#));
        }
        return Ok(json_resp(StatusCode::OK, r#"{"ok":true}"#));
    }
    if method == hyper::Method::OPTIONS {
        return Ok(Response::builder()
            .status(200)
            .header("access-control-allow-origin", "*")
            .header("access-control-allow-methods", "GET,POST,OPTIONS")
            .header("access-control-allow-headers", "Content-Type,Authorization,x-api-key,anthropic-version,anthropic-beta")
            .body(full(Bytes::new()))
            .unwrap());
    }

    // ── Read body ───────────────────────────────────────────────────────
    let (parts, body) = req.into_parts();
    let body_bytes = match body.collect().await {
        Ok(c) => c.to_bytes(),
        Err(e) => {
            error!(error = %e, "Body read failed");
            return Ok(json_resp(StatusCode::BAD_REQUEST, r#"{"error":"body read failed"}"#));
        }
    };

    let mut parsed: Option<serde_json::Value> = serde_json::from_slice(&body_bytes).ok();
    let model = parsed.as_ref().and_then(|p| p["model"].as_str()).unwrap_or("?").to_string();

    // ── Compress tool_result blocks ─────────────────────────────────────
    let comp_stats = if let Some(ref mut body_val) = parsed {
        compress::compress_request_body(body_val)
    } else {
        compress::CompressionStats::default()
    };

    if comp_stats.tool_results_processed > 0 {
        info!(
            tool_results = comp_stats.tool_results_processed,
            original = comp_stats.original_bytes,
            compressed = comp_stats.compressed_bytes,
            savings_pct = format!("{:.0}", comp_stats.savings_percent()),
            "Compressed context"
        );
        let mut st = state.stats.lock().await;
        st.tool_results_compressed += comp_stats.tool_results_processed as u64;
        st.compressed_original_bytes += comp_stats.original_bytes as u64;
        st.compressed_final_bytes += comp_stats.compressed_bytes as u64;
    }

    // ── Context window management ─────────────────────────────────────
    let ctx_action = if let Some(ref body_val) = parsed {
        context::analyze(body_val)
    } else {
        context::ContextAction { force_provider: None, action: "none", truncation_limit: 12_000 }
    };

    // Tier 4: trim if over 120K
    if ctx_action.action == "trim-and-claude" {
        if let Some(ref mut body_val) = parsed {
            context::trim_messages(body_val, 150_000);
        }
    }

    // ── Memory injection ────────────────────────────────────────────────
    if let (Some(ref db), Some(ref mut body_val)) = (&state.db, &mut parsed) {
        let project = memory::detect_project(body_val);
        memory::inject_memories(db, body_val, &project).await;
    }

    // ── Route decision ──────────────────────────────────────────────────
    // Context action can override the provider
    let route = if let Some(forced_by_context) = ctx_action.force_provider {
        router::RouteDecision {
            provider: forced_by_context,
            reason: ctx_action.action,
            estimated_tokens: parsed.as_ref().map(|b| tokens::estimate_tokens(b)).unwrap_or(0),
        }
    } else {
        router::decide(&parsed, &state.config.force_provider)
    };

    info!(
        provider = route.provider,
        model = %model,
        reason = route.reason,
        est_tokens = route.estimated_tokens,
        "Routing"
    );

    // Re-serialize if compressed, otherwise use original bytes
    let final_body = if comp_stats.tool_results_processed > 0 {
        Bytes::from(serde_json::to_vec(parsed.as_ref().unwrap()).unwrap_or_else(|_| body_bytes.to_vec()))
    } else {
        body_bytes.clone()
    };

    // ── Forward ─────────────────────────────────────────────────────────
    let comp_orig = comp_stats.original_bytes as u64;
    let comp_final = comp_stats.compressed_bytes as u64;

    if route.provider == "claude" {
        forward::claude(&parts, &final_body, &path, &model, route.reason, comp_orig, comp_final, &state).await
    } else {
        forward::deepseek(&final_body, &path, &parsed, &model, route.reason, comp_orig, comp_final, &state).await
    }
}
