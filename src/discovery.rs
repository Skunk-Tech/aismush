//! Auto-discovery of local model servers on known ports.
//!
//! Probes Ollama, LM Studio, llama.cpp, vLLM, Jan, text-generation-webui, KoboldCpp
//! on their default ports and registers them in the provider registry.

use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::provider::{ProviderConfig, ProviderKind, ProviderRegistry, Tier};
use crate::state::HttpClient;

/// Known local model servers with their default ports and detection methods.
struct KnownServer {
    name: &'static str,
    port: u16,
    health_path: &'static str,
    models_path: &'static str,
    /// How to extract model name from the health/models response
    detect: DetectMethod,
}

enum DetectMethod {
    /// GET /api/tags returns {"models": [{"name": "..."}]}
    OllamaTags,
    /// GET /v1/models returns {"data": [{"id": "..."}]}
    OpenAIModels,
    /// GET /health returns 200 OK, models at /v1/models
    HealthThenModels,
    /// GET /api/v1/model returns {"result": "..."}
    KoboldModel,
}

const KNOWN_SERVERS: &[KnownServer] = &[
    KnownServer { name: "ollama", port: 11434, health_path: "/api/tags", models_path: "/api/tags", detect: DetectMethod::OllamaTags },
    KnownServer { name: "lm-studio", port: 1234, health_path: "/v1/models", models_path: "/v1/models", detect: DetectMethod::OpenAIModels },
    KnownServer { name: "llama-cpp", port: 8080, health_path: "/health", models_path: "/v1/models", detect: DetectMethod::HealthThenModels },
    KnownServer { name: "vllm", port: 8000, health_path: "/health", models_path: "/v1/models", detect: DetectMethod::HealthThenModels },
    KnownServer { name: "jan", port: 1337, health_path: "/v1/models", models_path: "/v1/models", detect: DetectMethod::OpenAIModels },
    KnownServer { name: "text-gen-webui", port: 5000, health_path: "/v1/models", models_path: "/v1/models", detect: DetectMethod::OpenAIModels },
    KnownServer { name: "koboldcpp", port: 5001, health_path: "/api/v1/model", models_path: "/api/v1/model", detect: DetectMethod::KoboldModel },
];

/// Result of probing a local server.
#[derive(Debug)]
pub struct DiscoveredServer {
    pub name: String,
    pub url: String,
    pub model: String,
    pub context_window: usize,
}

/// Probe all known ports and return discovered servers.
/// Each probe has a 1.5s timeout, so worst case is ~10.5s for all 7 servers.
/// Runs in background every 60s so this is acceptable.
pub async fn discover_local_servers(client: &HttpClient) -> Vec<DiscoveredServer> {
    let mut discovered = Vec::new();

    for server in KNOWN_SERVERS {
        let base_url = format!("http://127.0.0.1:{}", server.port);
        let health_url = format!("{}{}", base_url, server.health_path);

        match probe_with_timeout(client, &health_url).await {
            Some(body) => {
                let (model, ctx) = extract_model_info(server, &body);
                if !model.is_empty() {
                    info!(server = server.name, port = server.port, model = %model, "Discovered local model server");
                    discovered.push(DiscoveredServer {
                        name: server.name.to_string(),
                        url: base_url,
                        model,
                        context_window: ctx,
                    });
                } else {
                    debug!(server = server.name, port = server.port, "Server responding but no models found");
                }
            }
            None => {
                debug!(server = server.name, port = server.port, "Not responding");
            }
        }
    }

    discovered
}

/// Make a GET request with a short timeout.
async fn probe_with_timeout(client: &HttpClient, url: &str) -> Option<String> {
    use http_body_util::BodyExt;

    let req = hyper::Request::builder()
        .method(hyper::Method::GET)
        .uri(url)
        .header("host", "127.0.0.1")
        .body(http_body_util::Full::new(bytes::Bytes::new()).map_err(|n| match n {}).boxed())
        .ok()?;

    let result = tokio::time::timeout(
        Duration::from_millis(1500),
        client.request(req),
    ).await;

    match result {
        Ok(Ok(resp)) => {
            if resp.status().is_success() {
                let body = resp.into_body().collect().await.ok()?;
                String::from_utf8(body.to_bytes().to_vec()).ok()
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Extract model name and context window from server response.
fn extract_model_info(server: &KnownServer, body: &str) -> (String, usize) {
    let Ok(json) = serde_json::from_str::<serde_json::Value>(body) else {
        return (String::new(), 32_000);
    };

    match server.detect {
        DetectMethod::OllamaTags => {
            // {"models": [{"name": "qwen3:8b", "details": {"parameter_size": "8B"}}]}
            if let Some(models) = json.get("models").and_then(|m| m.as_array()) {
                if let Some(first) = models.first() {
                    let name = first.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
                    // Ollama default context is typically 2048-8192, but many models support more
                    // We'll use a conservative 32K default, can be refined by /api/show
                    return (name, 32_000);
                }
            }
            (String::new(), 32_000)
        }
        DetectMethod::OpenAIModels => {
            // {"data": [{"id": "model-name"}]}
            if let Some(data) = json.get("data").and_then(|d| d.as_array()) {
                if let Some(first) = data.first() {
                    let id = first.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string();
                    return (id, 32_000);
                }
            }
            (String::new(), 32_000)
        }
        DetectMethod::HealthThenModels => {
            // Health check passed, try to extract model from response or use default
            if let Some(data) = json.get("data").and_then(|d| d.as_array()) {
                if let Some(first) = data.first() {
                    let id = first.get("id").and_then(|i| i.as_str()).unwrap_or("default").to_string();
                    return (id, 32_000);
                }
            }
            // Some servers return status only on /health
            ("default".to_string(), 32_000)
        }
        DetectMethod::KoboldModel => {
            // {"result": "model-name"}
            let name = json.get("result").and_then(|r| r.as_str()).unwrap_or("").to_string();
            (name, 4_096) // KoboldCpp typically uses smaller contexts
        }
    }
}

/// Register discovered servers into the provider registry.
pub async fn register_discovered(registry: &RwLock<ProviderRegistry>, servers: &[DiscoveredServer]) {
    let mut reg = registry.write().await;
    for server in servers {
        let id = format!("local-{}", server.name);

        // Don't re-register if already present
        if reg.has_provider(&id) {
            // Update health to healthy since we just discovered it
            if let Some(p) = reg.get(&id) {
                p.mark_healthy().await;
            }
            continue;
        }

        reg.register(
            ProviderConfig::new(
                &id,
                ProviderKind::OpenAICompat,
                &server.url,
                None, // local servers don't need API keys
                &server.model,
            )
            .with_tier(Tier::Free)
            .with_pricing(0.0, 0.0)
            .with_context_window(server.context_window)
            .with_max_output(4096)
            .with_tools(false) // conservative — most local models don't handle tools well
            .with_auto_discovered(true)
        );

        info!(id = %id, model = %server.model, url = %server.url, "Registered auto-discovered provider");
    }
}

/// Background task: periodically re-discover local servers and update health.
pub async fn discovery_loop(
    client: HttpClient,
    registry: Arc<RwLock<ProviderRegistry>>,
) {
    let mut interval = tokio::time::interval(Duration::from_secs(60));
    interval.tick().await; // skip first immediate tick

    loop {
        interval.tick().await;

        let discovered = discover_local_servers(&client).await;
        register_discovered(&registry, &discovered).await;

        // Mark non-responding discovered providers as down
        let reg = registry.read().await;
        let discovered_names: Vec<String> = discovered.iter().map(|d| format!("local-{}", d.name)).collect();
        for provider in &reg.providers {
            if provider.auto_discovered && !discovered_names.contains(&provider.id) {
                if provider.is_healthy().await {
                    warn!(provider = %provider.id, "Auto-discovered provider no longer responding");
                    provider.mark_down().await;
                }
            }
        }
    }
}

/// Print a report of all providers for the --providers CLI flag.
pub async fn print_providers_report(registry: &ProviderRegistry) {
    let report = registry.status_report().await;

    println!();
    println!("  AISmush — Provider Registry");
    println!("  ───────────────────────────");
    println!();

    if report.is_empty() {
        println!("  No providers configured.");
        println!();
        return;
    }

    for p in &report {
        let health = if p.down { "DOWN" } else if p.healthy { "OK" } else { "DEGRADED" };
        let health_color = if p.down { "\x1b[31m" } else if p.healthy { "\x1b[32m" } else { "\x1b[33m" };
        let source = if p.auto_discovered { " (auto)" } else { "" };
        let pricing = if p.pricing.0 == 0.0 && p.pricing.1 == 0.0 {
            "free".to_string()
        } else {
            format!("${:.2}/${:.2} per M", p.pricing.0, p.pricing.1)
        };
        let tools = if p.supports_tools { "tools" } else { "no-tools" };

        println!("  {}{}\x1b[0m {} [{}]", health_color, health, p.id, p.tier);
        println!("    Model: {}  Context: {}K  {}", p.model, p.context_window / 1000, tools);
        println!("    URL: {}  Pricing: {}{}", p.base_url, pricing, source);
        println!();
    }
}
