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

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[tokio::main]
async fn main() {
    // Handle CLI flags before anything else
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("aismush {}", VERSION);
        return;
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("AISmush v{} — Hybrid Claude/DeepSeek Proxy", VERSION);
        println!();
        println!("USAGE:");
        println!("  aismush              Start the proxy server");
        println!("  aismush --version    Show version");
        println!("  aismush --status     Check if proxy is running");
        println!("  aismush --config     Show current configuration");
        println!("  aismush --upgrade    Upgrade to latest version");
        println!();
        println!("The proxy reads config from:");
        println!("  1. Environment variables (DEEPSEEK_API_KEY, PROXY_PORT)");
        println!("  2. ./config.json or ./.deepseek-proxy.json");
        println!("  3. ~/.hybrid-proxy/config.json");
        println!();
        println!("Dashboard: http://localhost:PORT/dashboard");
        return;
    }
    if args.iter().any(|a| a == "--status") {
        let cfg = config::ProxyConfig::load();
        match reqwest_lite(&format!("http://127.0.0.1:{}/stats", cfg.port)).await {
            Some(body) => {
                println!("AISmush is running on port {}", cfg.port);
                if let Ok(s) = serde_json::from_str::<serde_json::Value>(&body) {
                    println!("  Requests: {} (Claude: {}, DeepSeek: {})",
                        s["total_requests"], s["claude_turns"], s["deepseek_turns"]);
                    if let Some(cost) = s["actual_cost"].as_f64() {
                        println!("  Cost: ${:.4} (saved ${:.4}, {:.1}%)",
                            cost, s["savings"].as_f64().unwrap_or(0.0), s["savings_percent"].as_f64().unwrap_or(0.0));
                    }
                }
            }
            None => println!("AISmush is not running (port {})", cfg.port),
        }
        return;
    }
    if args.iter().any(|a| a == "--config") {
        let cfg = config::ProxyConfig::load();
        println!("DeepSeek Key: {}", if cfg.api_key.is_empty() { "(not set)".into() } else { format!("{}...", &cfg.api_key[..8.min(cfg.api_key.len())]) });
        println!("Port:         {}", cfg.port);
        println!("Mode:         {}", cfg.force_provider.as_deref().unwrap_or("hybrid"));
        println!("Verbose:      {}", cfg.verbose);
        println!("Data Dir:     {}", cfg.data_dir.display());
        println!("Database:     {}", cfg.db_path.display());
        return;
    }
    if args.iter().any(|a| a == "--upgrade") {
        upgrade().await;
        return;
    }

    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");

    // Check for updates on startup (visible to user before log redirect)
    tokio::spawn(async {
        check_for_updates().await;
    });

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

    // Periodic stats reporting (every 10 minutes)
    let stats_state = state.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(600)).await;
            report_stats(&stats_state).await;
        }
    });

    // Handle both SIGINT (Ctrl+C) and SIGTERM (kill)
    let shutdown = async {
        let ctrl_c = tokio::signal::ctrl_c();
        #[cfg(unix)]
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();
        #[cfg(unix)]
        {
            tokio::select! {
                _ = ctrl_c => {},
                _ = sigterm.recv() => {},
            }
        }
        #[cfg(not(unix))]
        ctrl_c.await.ok();
    };
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
                report_stats(&state).await;
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
    let project_path = parsed.as_ref().map(|b| memory::detect_project(b)).unwrap_or_else(|| "default".to_string());

    // ── Compress tool_result blocks (safe for both providers — only modifies
    //    content strings inside tool_result, never changes message structure) ──
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

    // ── Memory injection (safe — prepends to system prompt, never touches messages) ──
    if let (Some(ref db), Some(ref mut body_val)) = (&state.db, &mut parsed) {
        memory::inject_memories(db, body_val, &project_path).await;
    }

    // ── Route decision ──────────────────────────────────────────────────
    let mut route = router::decide(&parsed, &state.config.force_provider);

    // ── Context window management ─────────────────────────────────────
    // ONLY for DeepSeek turns. Claude has 200K window and doesn't need trimming.
    // Trimming messages breaks tool_use/tool_result pairs → causes 400 errors on Claude.
    let mut body_modified = comp_stats.original_bytes != comp_stats.compressed_bytes; // Only true if content actually changed
    if route.provider == "deepseek" {
        if let Some(ref mut body_val) = parsed {
            let ctx = context::prepare(body_val, "deepseek");
            if ctx.body_modified { body_modified = true; }
            if let Some(forced) = ctx.force_provider {
                route.provider = forced;
                route.reason = ctx.action;
            }
            if context::ensure_fits(body_val, route.provider) {
                body_modified = true;
            }
        }
    }

    if let Some(ref body_val) = parsed {
        route.estimated_tokens = tokens::estimate_tokens(body_val);
    }

    let display_model = if route.provider == "deepseek" { "deepseek-chat".to_string() } else { model.clone() };
    info!(
        provider = route.provider,
        model = %display_model,
        reason = route.reason,
        est_tokens = route.estimated_tokens,
        "Routing"
    );

    // Re-serialize if body was modified (compression or context trimming)
    let final_body = if body_modified {
        Bytes::from(serde_json::to_vec(parsed.as_ref().unwrap()).unwrap_or_else(|_| body_bytes.to_vec()))
    } else {
        body_bytes.clone()
    };

    // ── Forward ─────────────────────────────────────────────────────────
    let comp_orig = comp_stats.original_bytes as u64;
    let comp_final = comp_stats.compressed_bytes as u64;

    if route.provider == "claude" {
        forward::claude(&parts, &final_body, &path, &model, route.reason, &project_path, comp_orig, comp_final, &state).await
    } else {
        forward::deepseek(&final_body, &path, &parsed, "deepseek-chat", route.reason, &project_path, comp_orig, comp_final, &state).await
    }
}

/// Tiny HTTP GET for --status check (localhost only).
async fn reqwest_lite(url: &str) -> Option<String> {
    let port = config::ProxyConfig::load().port;
    let addr = format!("127.0.0.1:{}", port);
    let stream = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        tokio::net::TcpStream::connect(&addr),
    ).await.ok()?.ok()?;

    let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream)).await.ok()?;
    tokio::spawn(conn);

    let req = Request::builder()
        .uri(url)
        .body(Full::new(Bytes::new()).map_err(|n| -> hyper::Error { match n {} }).boxed())
        .ok()?;

    let resp = sender.send_request(req).await.ok()?;
    let body = resp.into_body().collect().await.ok()?;
    Some(String::from_utf8_lossy(&body.to_bytes()).to_string())
}

/// Check GitHub for newer version on startup.
async fn check_for_updates() {
    // Small delay so it doesn't slow startup
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    let url = "https://api.github.com/repos/Skunk-Tech/aismush/releases/latest";
    let output = tokio::process::Command::new("curl")
        .args(["-sfL", "-H", "Accept: application/vnd.github.v3+json", url])
        .output()
        .await;

    if let Ok(out) = output {
        if let Ok(body) = String::from_utf8(out.stdout) {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&body) {
                if let Some(tag) = json["tag_name"].as_str() {
                    let latest = tag.trim_start_matches('v');
                    if latest != VERSION && !latest.is_empty() {
                        eprintln!("  [AISmush] Update available: v{} → v{} (run: aismush --upgrade)", VERSION, latest);
                        info!("Update available: v{} -> v{}", VERSION, latest);

                        // Write flag file so start script can show a banner
                        let flag_path = config::ProxyConfig::load().data_dir.join("update-available");
                        let _ = std::fs::write(&flag_path, format!("{}\n", latest));
                    } else {
                        // No update — remove flag if it exists
                        let flag_path = config::ProxyConfig::load().data_dir.join("update-available");
                        let _ = std::fs::remove_file(&flag_path);
                    }
                }
            }
        }
    }
}

/// Report anonymous stats to the global dashboard (opt-in via config).
async fn report_stats(state: &Arc<ProxyState>) {
    let stats = state.stats.lock().await;
    let body = serde_json::json!({
        "requests": stats.total_requests,
        "claude_turns": stats.claude_turns,
        "deepseek_turns": stats.deepseek_turns,
        "savings": 0, // Will be populated from DB in future
        "compressed_bytes": stats.compressed_original_bytes - stats.compressed_final_bytes,
        "version": VERSION,
    });
    drop(stats);

    let _ = tokio::process::Command::new("curl")
        .args([
            "-sf", "-X", "POST",
            "https://aismush.us.com/api/report",
            "-H", "Content-Type: application/json",
            "-d", &body.to_string(),
        ])
        .output()
        .await;
}

/// Self-upgrade by running the install script.
async fn upgrade() {
    println!("AISmush v{} — checking for updates...", VERSION);
    println!();

    // Use the install script which handles platform detection and download
    let status = tokio::process::Command::new("bash")
        .arg("-c")
        .arg("curl -fsSL https://raw.githubusercontent.com/Skunk-Tech/aismush/main/install.sh | bash")
        .status()
        .await;

    match status {
        Ok(s) if s.success() => {
            println!();
            println!("Upgrade complete. Run 'aismush --version' to verify.");
        }
        Ok(s) => {
            eprintln!("Upgrade failed (exit code {:?})", s.code());
            eprintln!("Try manually: curl -fsSL https://raw.githubusercontent.com/Skunk-Tech/aismush/main/install.sh | bash");
        }
        Err(e) => {
            eprintln!("Upgrade failed: {}", e);
            eprintln!("Try manually: curl -fsSL https://raw.githubusercontent.com/Skunk-Tech/aismush/main/install.sh | bash");
        }
    }
}

