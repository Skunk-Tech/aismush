mod capture;
mod compress;
mod config;
mod context;
mod cost;
mod dashboard;
mod db;
mod deps;
mod embeddings;
mod forward;
mod hashes;
mod memory;
mod prompts;
mod router;
mod scan;
mod search;
mod state;
mod summarize;
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
        println!("AISmush v{} — Smart AI Coding Proxy", VERSION);
        println!();
        println!("USAGE:");
        println!("  aismush              Start the proxy server (smart routing)");
        println!("  aismush --direct     Start in direct mode (Claude only, no DeepSeek)");
        println!("  aismush --scan       Scan codebase and generate optimized agents");
        println!("  aismush --version    Show version");
        println!("  aismush --status     Check if proxy is running");
        println!("  aismush --config     Show current configuration");
        println!("  aismush --embeddings Start with semantic search enabled (loads 90MB model)");
        println!("  aismush --upgrade    Upgrade to latest version");
        println!("  aismush --uninstall  Uninstall AISmush");
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
    if args.iter().any(|a| a == "--uninstall") {
        uninstall().await;
        return;
    }
    if args.iter().any(|a| a == "--search") {
        run_search(&args).await;
        return;
    }
    if args.iter().any(|a| a == "--scan") {
        run_scan(&args).await;
        return;
    }

    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");

    // Check for updates BEFORE server starts (prints to stderr before log redirect)
    check_for_updates().await;

    let mut cfg = config::ProxyConfig::load();

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

    // --direct flag forces Claude-only mode
    if args.iter().any(|a| a == "--direct") {
        cfg.force_provider = Some("claude".to_string());
        info!("Direct mode (Claude only — compression + memory + agents still active)");
    } else if let Some(ref fp) = cfg.force_provider {
        info!(provider = %fp, "Forced provider");
    } else {
        info!("Smart routing mode");
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

    // Start with no embedder — initialize it in background so the server can listen immediately
    // Render dashboard HTML once at startup (static template, only port varies)
    let dashboard_html = dashboard::render(cfg.port).await;
    let state = ProxyState::new(cfg.clone(), client, database, None, dashboard_html);

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], cfg.port));
    let listener = TcpListener::bind(addr).await.expect("Failed to bind port");
    info!(port = cfg.port, "Listening");
    info!("Dashboard: http://127.0.0.1:{}/dashboard", cfg.port);

    // Embeddings are opt-in (--embeddings flag or AISMUSH_EMBEDDINGS=1)
    // The 90MB model uses significant CPU/RAM — only load when user explicitly wants semantic search
    let embeddings_enabled = args.iter().any(|a| a == "--embeddings")
        || std::env::var("AISMUSH_EMBEDDINGS").unwrap_or_default() == "1";

    if embeddings_enabled {
        let embed_state = state.clone();
        let embed_data_dir = cfg.data_dir.clone();
        tokio::spawn(async move {
            match embeddings::EmbeddingEngine::new(&embed_data_dir).await {
                Ok(engine) => {
                    *embed_state.embedder.write().await = Some(engine);
                    info!("Embedding engine loaded (background)");
                }
                Err(e) => {
                    warn!("Embedding engine unavailable: {} (search will use keywords only)", e);
                }
            }
        });
    } else {
        info!("Embeddings disabled (use --embeddings or AISMUSH_EMBEDDINGS=1 to enable semantic search)");
    }

    // Periodic stats reporting (every 10 minutes)
    let stats_state = state.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(600)).await;
            report_stats(&stats_state).await;
        }
    });

    // Periodic database cleanup (every 6 hours)
    let cleanup_state = state.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(6 * 3600)).await;
            if let Some(ref db) = cleanup_state.db {
                memory::decay_and_prune(db).await;
            }
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

/// Extract the maximum blast radius score from file paths in the last message's tool_use inputs.
async fn extract_blast_radius_score(db: &db::Db, project_path: &str, body: &serde_json::Value) -> Option<f64> {
    let messages = body.get("messages")?.as_array()?;
    let last = messages.last()?;
    let content = last.get("content")?.as_array()?;

    let mut max_score: Option<f64> = None;

    for block in content {
        // Look at tool_use inputs for file_path fields
        if block.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
            if let Some(input) = block.get("input") {
                if let Some(path) = input.get("file_path").and_then(|p| p.as_str()) {
                    // Normalize: strip leading ./ or project path prefix
                    let rel_path = path.trim_start_matches("./");
                    if let Some(br) = deps::lookup_blast_radius(db, project_path, rel_path).await {
                        max_score = Some(max_score.unwrap_or(0.0).max(br.score));
                    }
                }
            }
        }
    }

    max_score
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
            return Ok(Response::builder().status(200).header("content-type", "text/html").body(full(state.dashboard_html.clone())).unwrap());
        }
        if path.starts_with("/search") {
            let query = path.split("q=").nth(1)
                .and_then(|q| q.split('&').next())
                .map(|q| urlencoding_decode(q))
                .unwrap_or_default();
            let project = path.split("project=").nth(1)
                .and_then(|p| p.split('&').next())
                .map(|p| urlencoding_decode(p));
            let embedder_guard = state.embedder.read().await;
            let results = search::search(
                state.db.as_ref().unwrap_or(&state.db.clone().unwrap()),
                embedder_guard.as_ref(),
                &query,
                project.as_deref(),
                20,
            ).await;
            return Ok(json_resp(StatusCode::OK, &serde_json::to_string(&results).unwrap()));
        }
        if path.starts_with("/conversation/") {
            let id: i64 = path.trim_start_matches("/conversation/").parse().unwrap_or(0);
            if let Some(ref db) = state.db {
                let turns = search::get_conversation(db, id).await;
                return Ok(json_resp(StatusCode::OK, &serde_json::to_string(&turns).unwrap()));
            }
            return Ok(json_resp(StatusCode::OK, "[]"));
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
    let model = parsed.as_ref()
        .and_then(|p| p["model"].as_str())
        .unwrap_or("claude")
        .to_string();
    let project_path = parsed.as_ref().map(|b| memory::detect_project(b)).unwrap_or_else(|| "default".to_string());

    // ── Extract observations from tool_use blocks (non-blocking) ────────
    if let (Some(ref db), Some(ref body_val)) = (&state.db, &parsed) {
        let observations = memory::extract_observations(body_val);
        if !observations.is_empty() {
            let db = db.clone();
            let project = project_path.clone();
            tokio::spawn(async move {
                memory::store_observations(&db, &project, observations).await;
            });
        }
    }

    // ── Route decision (before any body modification) ──────────────────
    let mut route_preview = router::decide(&parsed, &state.config.force_provider);

    // ── Blast radius check — override routing for high-impact files ────
    if route_preview.provider == "deepseek" {
        if let (Some(ref db), Some(ref body_val)) = (&state.db, &parsed) {
            let blast_score = extract_blast_radius_score(db, &project_path, body_val).await;
            let threshold: f64 = std::env::var("AISMUSH_BLAST_THRESHOLD")
                .ok().and_then(|v| v.parse().ok()).unwrap_or(0.7);
            if let Some(score) = blast_score {
                if score > threshold {
                    route_preview = router::RouteDecision {
                        provider: "claude",
                        reason: "high-blast-radius",
                        estimated_tokens: route_preview.estimated_tokens,
                    };
                    info!(score = format!("{:.2}", score), "Blast radius override → Claude");
                }
            }
        }
    }

    // ── Compress tool_result blocks (DeepSeek only — Claude gets original bytes) ──
    let comp_stats = if route_preview.provider == "deepseek" {
        if let Some(ref mut body_val) = parsed {
            compress::compress_with_summaries(body_val, state.db.as_ref())
        } else {
            compress::CompressionStats::default()
        }
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

    // ── Memory injection (DeepSeek only for now — Claude gets unmodified body) ──
    if route_preview.provider == "deepseek" {
        if let (Some(ref db), Some(ref mut body_val)) = (&state.db, &mut parsed) {
            memory::inject_memories(db, body_val, &project_path).await;
        }
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

    let comp_orig = comp_stats.original_bytes as u64;
    let comp_final = comp_stats.compressed_bytes as u64;

    if route.provider == "claude" {
        forward::claude(&parts, &final_body, &path, &model, route.reason, &project_path, comp_orig, comp_final, parsed.as_ref(), &state).await
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

/// Check GitHub for newer version on startup (3 second timeout).
async fn check_for_updates() {
    let url = "https://api.github.com/repos/Skunk-Tech/aismush/releases/latest";
    let output = tokio::process::Command::new("curl")
        .args(["-sfL", "--max-time", "3", "-H", "Accept: application/vnd.github.v3+json", url])
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

/// Report anonymous stats to the global dashboard.
/// Sends only the delta since the last report, not cumulative totals.
async fn report_stats(state: &Arc<ProxyState>) {
    // Read from DB for accurate cumulative stats (survives restarts)
    let body = if let Some(ref database) = state.db {
        let db_stats = db::get_stats(database).await;
        let comp_orig = db_stats["compressed_original_bytes"].as_i64().unwrap_or(0);
        let comp_final = db_stats["compressed_final_bytes"].as_i64().unwrap_or(0);
        let comp_tokens_saved = (comp_orig - comp_final) / 4;
        let comp_cost_saved = comp_tokens_saved as f64 * 3.0 / 1_000_000.0;

        // Current cumulative values
        let cur_requests = db_stats["total_requests"].as_i64().unwrap_or(0);
        let cur_claude = db_stats["claude_turns"].as_i64().unwrap_or(0);
        let cur_deepseek = db_stats["deepseek_turns"].as_i64().unwrap_or(0);
        let cur_routing_savings = db_stats["savings"].as_f64().unwrap_or(0.0);
        let cur_comp_savings = (comp_cost_saved * 10000.0).round() / 10000.0;
        let cur_comp_tokens = comp_tokens_saved;

        // Compute deltas against last report
        let mut last = state.last_reported.lock().await;
        let delta_requests = cur_requests - last.requests;
        let delta_claude = cur_claude - last.claude_turns;
        let delta_deepseek = cur_deepseek - last.deepseek_turns;
        let delta_routing = cur_routing_savings - last.routing_savings;
        let delta_comp_savings = cur_comp_savings - last.compression_savings;
        let delta_comp_tokens = cur_comp_tokens - last.compressed_tokens;

        // Update last-reported snapshot
        last.requests = cur_requests;
        last.claude_turns = cur_claude;
        last.deepseek_turns = cur_deepseek;
        last.routing_savings = cur_routing_savings;
        last.compression_savings = cur_comp_savings;
        last.compressed_tokens = cur_comp_tokens;
        drop(last);

        serde_json::json!({
            "requests": delta_requests.max(0),
            "claude_turns": delta_claude.max(0),
            "deepseek_turns": delta_deepseek.max(0),
            "routing_savings": if delta_routing > 0.0 { (delta_routing * 10000.0).round() / 10000.0 } else { 0.0 },
            "compression_savings": if delta_comp_savings > 0.0 { (delta_comp_savings * 10000.0).round() / 10000.0 } else { 0.0 },
            "compressed_tokens": delta_comp_tokens.max(0),
            "instance_id": state.instance_id,
            "version": VERSION,
        })
    } else {
        // Fallback to in-memory if no DB
        let stats = state.stats.lock().await;
        let body = serde_json::json!({
            "requests": stats.total_requests,
            "claude_turns": stats.claude_turns,
            "deepseek_turns": stats.deepseek_turns,
            "routing_savings": 0,
            "compression_savings": 0,
            "compressed_tokens": 0,
            "instance_id": state.instance_id,
            "version": VERSION,
        });
        drop(stats);
        body
    };

    // Skip if nothing new to report
    if body["requests"].as_i64().unwrap_or(0) == 0 {
        return;
    }

    let body_str = body.to_string();
    debug!(body = %body_str, "Reporting stats to global dashboard");

    let output = tokio::process::Command::new("curl")
        .args([
            "-sf", "-X", "POST",
            "https://aismush.us.com/api/report",
            "-H", "Content-Type: application/json",
            "-d", &body_str,
        ])
        .output()
        .await;

    match output {
        Ok(o) if o.status.success() => {
            info!("Stats reported to global dashboard");
        }
        Ok(o) => {
            warn!("Stats report failed: {}", String::from_utf8_lossy(&o.stderr));
        }
        Err(e) => {
            warn!(error = %e, "Stats report failed");
        }
    }
}

/// Scan a project and generate optimized Claude Code agents via AI analysis.
fn urlencoding_decode(s: &str) -> String {
    s.replace('+', " ").replace("%20", " ").replace("%2F", "/")
}

/// Search past conversations from CLI.
async fn run_search(args: &[String]) {
    let query = args.iter()
        .position(|a| a == "--search")
        .and_then(|i| args.get(i + 1))
        .map(|s| s.as_str())
        .unwrap_or("");

    if query.is_empty() {
        eprintln!("Usage: aismush --search \"your query\"");
        return;
    }

    let cfg = config::ProxyConfig::load();

    // Open DB
    let db = match db::open(&cfg.db_path) {
        Ok(d) => d,
        Err(e) => { eprintln!("Database error: {}", e); return; }
    };

    // Try to load embedder
    let embedder = match embeddings::EmbeddingEngine::new(&cfg.data_dir).await {
        Ok(e) => Some(e),
        Err(_) => None,
    };

    let results = search::search(&db, embedder.as_ref(), query, None, 10).await;

    if results.is_empty() {
        eprintln!("  No results for \"{}\"", query);
        return;
    }

    eprintln!("  {} results for \"{}\":\n", results.len(), query);
    for (i, r) in results.iter().enumerate() {
        let time = chrono_format(r.timestamp);
        eprintln!("  {}. [{}] {} (score: {:.2})", i + 1, time, r.project_path, r.similarity_score);
        eprintln!("     You: {}", truncate_str(&r.user_message, 80));
        eprintln!("     AI:  {}", truncate_str(&r.assistant_snippet, 120));
        if !r.tools_used.is_empty() {
            eprintln!("     Tools: {}", r.tools_used.join(", "));
        }
        eprintln!();
    }
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.len() <= max { return s.to_string(); }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) { end -= 1; }
    format!("{}...", &s[..end])
}

fn chrono_format(ts: i64) -> String {
    // Simple human-readable time
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let diff = now - ts;
    if diff < 60 { "just now".to_string() }
    else if diff < 3600 { format!("{}m ago", diff / 60) }
    else if diff < 86400 { format!("{}h ago", diff / 3600) }
    else { format!("{}d ago", diff / 86400) }
}

async fn run_scan(args: &[String]) {
    let project_path = args.iter()
        .position(|a| a == "--scan")
        .and_then(|i| args.get(i + 1))
        .map(|p| std::path::PathBuf::from(p))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")));

    let force = args.iter().any(|a| a == "--force");
    let cfg = config::ProxyConfig::load();

    eprintln!();
    eprintln!("  AISmush Scanner v{}", VERSION);
    eprintln!("  ──────────────────────");
    eprintln!("  Project: {}", project_path.display());
    eprintln!();

    // Check if proxy is running, if not start it temporarily
    let mut started_proxy = false;
    let proxy_running = tokio::net::TcpStream::connect(format!("127.0.0.1:{}", cfg.port))
        .await
        .is_ok();

    if !proxy_running {
        eprintln!("  Starting proxy temporarily for AI analysis...");
        let exe = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("aismush"));
        let _child = tokio::process::Command::new(&exe)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        // Wait for proxy to start
        for _ in 0..20 {
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            if tokio::net::TcpStream::connect(format!("127.0.0.1:{}", cfg.port)).await.is_ok() {
                started_proxy = true;
                break;
            }
        }
        if !started_proxy {
            eprintln!("  Error: Could not start proxy. Run 'aismush' in another terminal first.");
            return;
        }
        eprintln!("  Proxy started on port {}", cfg.port);
    }

    // Step 1: Scan filesystem
    eprintln!("  [1/6] Scanning filesystem...");
    let profile = scan::scan_project(&project_path);
    eprintln!("        Found {} files", profile.total_files);
    eprintln!("        Sampled {} config files + {} code files",
        profile.config_files.len(), profile.sampled_code.len());

    // Detect existing artifacts
    let existing = scan::detect_existing(&project_path);
    if !existing.agents.is_empty() {
        eprintln!("        Existing agents: {} (will skip, use --force to overwrite)", existing.agents.len());
    }

    // Steps 2-6: AI pipeline
    // If no DeepSeek key, use Claude (works in direct mode)
    let use_claude = cfg.api_key.is_empty() || cfg.force_provider.as_deref() == Some("claude");
    if use_claude {
        eprintln!("        Using Claude for analysis (no DeepSeek key or direct mode)");
    } else {
        eprintln!("        Using DeepSeek for analysis (cost: ~$0.03)");
    }
    match scan::run_pipeline(&profile, &existing, cfg.port, &cfg.api_key, use_claude).await {
        Ok(result) => {
            // Write artifacts
            eprintln!("  [6/6] Writing artifacts...");
            let summary = scan::write_artifacts(&project_path, &result, force);

            eprintln!();
            eprintln!("  Summary:");
            eprintln!("    Agents:    {} created, {} skipped", summary.agents_created, summary.agents_skipped);
            eprintln!("    Skills:    {} created, {} skipped", summary.skills_created, summary.skills_skipped);
            if summary.claude_md_created {
                eprintln!("    CLAUDE.md: Created");
            } else if summary.claude_md_skipped {
                eprintln!("    CLAUDE.md: Skipped (exists, use --force to overwrite)");
            }
            eprintln!();
            eprintln!("  Done! Run 'aismush-start' to use the generated agents.");
        }
        Err(e) => {
            eprintln!();
            eprintln!("  Error: {}", e);
            eprintln!("  Make sure the proxy is running and DeepSeek API key is configured.");
        }
    }

    // Stop proxy if we started it
    if started_proxy {
        eprintln!("  Stopping temporary proxy...");
        // Send shutdown signal
        let _ = tokio::process::Command::new("curl")
            .args(["-sf", &format!("http://localhost:{}/health", cfg.port)])
            .output()
            .await;
        // Kill by port
        let _ = tokio::process::Command::new("bash")
            .args(["-c", &format!("lsof -ti:{} | xargs kill 2>/dev/null", cfg.port)])
            .output()
            .await;
    }

    eprintln!();
}

/// Self-upgrade by running the install script.
async fn upgrade() {
    println!("AISmush v{} — checking for updates...", VERSION);
    println!();

    #[cfg(unix)]
    let status = tokio::process::Command::new("bash")
        .arg("-c")
        .arg("curl -fsSL https://raw.githubusercontent.com/Skunk-Tech/aismush/main/install.sh | bash")
        .status()
        .await;

    #[cfg(windows)]
    let status = tokio::process::Command::new("powershell")
        .args(["-ExecutionPolicy", "Bypass", "-Command",
            "irm https://raw.githubusercontent.com/Skunk-Tech/aismush/main/install.ps1 | iex"])
        .status()
        .await;

    match status {
        Ok(s) if s.success() => {
            println!();
            println!("Upgrade complete. Run 'aismush --version' to verify.");
        }
        Ok(s) => {
            eprintln!("Upgrade failed (exit code {:?})", s.code());
            #[cfg(unix)]
            eprintln!("Try manually: curl -fsSL https://raw.githubusercontent.com/Skunk-Tech/aismush/main/install.sh | bash");
            #[cfg(windows)]
            eprintln!("Try manually: irm https://raw.githubusercontent.com/Skunk-Tech/aismush/main/install.ps1 | iex");
        }
        Err(e) => {
            eprintln!("Upgrade failed: {}", e);
            #[cfg(unix)]
            eprintln!("Try manually: curl -fsSL https://raw.githubusercontent.com/Skunk-Tech/aismush/main/install.sh | bash");
            #[cfg(windows)]
            eprintln!("Try manually: irm https://raw.githubusercontent.com/Skunk-Tech/aismush/main/install.ps1 | iex");
        }
    }
}

/// Uninstall by delegating to the install script with --uninstall flag.
async fn uninstall() {
    println!("AISmush v{} — uninstalling...", VERSION);
    println!();

    #[cfg(unix)]
    let status = tokio::process::Command::new("bash")
        .arg("-c")
        .arg("curl -fsSL https://raw.githubusercontent.com/Skunk-Tech/aismush/main/install.sh | bash -s -- --uninstall")
        .status()
        .await;

    #[cfg(windows)]
    let status = tokio::process::Command::new("powershell")
        .args(["-ExecutionPolicy", "Bypass", "-Command",
            "& { irm https://raw.githubusercontent.com/Skunk-Tech/aismush/main/install.ps1 -OutFile $env:TEMP\\aismush-uninstall.ps1; & $env:TEMP\\aismush-uninstall.ps1 --uninstall }"])
        .status()
        .await;

    match status {
        Ok(s) if s.success() => {}
        Ok(_) | Err(_) => {
            eprintln!("Automatic uninstall failed. To uninstall manually:");
            #[cfg(unix)]
            {
                eprintln!("  rm ~/.local/bin/aismush ~/.local/bin/aismush-start");
                eprintln!("  rm -rf ~/.hybrid-proxy/");
            }
            #[cfg(windows)]
            {
                eprintln!("  Remove-Item -Recurse \"$env:LOCALAPPDATA\\AISmush\"");
                eprintln!("  Remove-Item -Recurse \"$env:USERPROFILE\\.hybrid-proxy\"");
            }
        }
    }
}

