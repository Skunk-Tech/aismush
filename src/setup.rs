//! Interactive provider setup with connection testing.
//!
//! Walks users through configuring DeepSeek, OpenRouter, and local model
//! servers, testing each connection before saving.

use bytes::Bytes;
use http_body_util::{combinators::BoxBody, BodyExt, Full};
use hyper::Request;
use serde_json::{json, Value};
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::time::Duration;

use crate::discovery;
use crate::state::HttpClient;

fn full(data: impl Into<Bytes>) -> BoxBody<Bytes, hyper::Error> {
    Full::new(data.into()).map_err(|n| match n {}).boxed()
}

pub async fn run_setup(client: &HttpClient) {
    println!();
    println!("  AISmush — Provider Setup");
    println!("  ────────────────────────");
    println!();

    let config_path = config_path();
    let mut config = load_existing_config(&config_path);

    // ── DeepSeek ──────────────────────────────────────────────────────
    println!("  \x1b[1m1. DeepSeek\x1b[0m (smart routing — saves ~90% on API costs)");
    let existing_ds = config.get("apiKey").and_then(|k| k.as_str()).unwrap_or("").to_string();
    if !existing_ds.is_empty() {
        println!("     Current key: {}...", &existing_ds[..8.min(existing_ds.len())]);
        let keep = prompt("     Keep existing key? [Y/n]: ");
        if keep.to_lowercase() != "n" {
            println!("     \x1b[32mKept.\x1b[0m Testing connection...");
            test_deepseek(client, &existing_ds).await;
        } else {
            setup_deepseek(client, &mut config).await;
        }
    } else {
        println!("     No key configured.");
        println!("     Get a free key at: https://platform.deepseek.com/api_keys");
        let key = prompt("     Paste your DeepSeek API key (or Enter to skip): ");
        if !key.is_empty() {
            println!("     Testing connection...");
            if test_deepseek(client, &key).await {
                config["apiKey"] = Value::String(key);
            }
        } else {
            println!("     \x1b[33mSkipped.\x1b[0m You can still use --direct mode (Claude only).");
        }
    }
    println!();

    // ── OpenRouter ────────────────────────────────────────────────────
    println!("  \x1b[1m2. OpenRouter\x1b[0m (access to 290+ models via one API key)");
    let existing_or = config.get("openrouterKey").and_then(|k| k.as_str()).unwrap_or("").to_string();
    if !existing_or.is_empty() {
        println!("     Current key: {}...", &existing_or[..8.min(existing_or.len())]);
        let keep = prompt("     Keep existing key? [Y/n]: ");
        if keep.to_lowercase() != "n" {
            println!("     \x1b[32mKept.\x1b[0m Testing connection...");
            test_openrouter(client, &existing_or).await;
        } else {
            setup_openrouter(client, &mut config).await;
        }
    } else {
        println!("     No key configured.");
        println!("     Get a key at: https://openrouter.ai/keys");
        let key = prompt("     Paste your OpenRouter API key (or Enter to skip): ");
        if !key.is_empty() {
            println!("     Testing connection...");
            if test_openrouter(client, &key).await {
                config["openrouterKey"] = Value::String(key);
            }
        } else {
            println!("     \x1b[33mSkipped.\x1b[0m");
        }
    }
    println!();

    // ── Local Models ──────────────────────────────────────────────────
    println!("  \x1b[1m3. Local Models\x1b[0m (free — zero API costs)");
    println!("     Scanning for local model servers...");
    let discovered = discovery::discover_local_servers(client).await;
    if discovered.is_empty() {
        println!("     \x1b[33mNo local servers found.\x1b[0m");
        println!("     Supported: Ollama (:11434), LM Studio (:1234), llama.cpp (:8080),");
        println!("                vLLM (:8000), Jan (:1337), text-gen-webui (:5000), KoboldCpp (:5001)");
        println!("     Start a server and run --setup again to configure it.");
    } else {
        for d in &discovered {
            println!("     \x1b[32mFound:\x1b[0m {} at {} (model: {})", d.name, d.url, d.model);
            // Test with a quick completion
            let test_ok = test_local(client, &d.url, &d.model).await;
            if test_ok {
                println!("     \x1b[32mConnection verified!\x1b[0m");
            }
        }
        // Save discovered servers to config
        let locals: Vec<Value> = discovered.iter().map(|d| {
            json!({"name": d.name, "url": d.url, "model": d.model})
        }).collect();
        config["local"] = Value::Array(locals);
        config["autoDiscoverLocal"] = Value::Bool(true);
    }
    println!();

    // ── Save config ───────────────────────────────────────────────────
    save_config(&config_path, &config);

    // ── Summary ───────────────────────────────────────────────────────
    println!("  \x1b[1mSetup Complete\x1b[0m");
    println!("  ─────────────");
    let has_ds = config.get("apiKey").and_then(|k| k.as_str()).map_or(false, |k| !k.is_empty());
    let has_or = config.get("openrouterKey").and_then(|k| k.as_str()).map_or(false, |k| !k.is_empty());
    let has_local = !discovered.is_empty();

    if has_ds { println!("  \x1b[32m✓\x1b[0m DeepSeek — smart routing enabled"); }
    else { println!("  \x1b[33m-\x1b[0m DeepSeek — not configured"); }
    if has_or { println!("  \x1b[32m✓\x1b[0m OpenRouter — 290+ models available"); }
    else { println!("  \x1b[33m-\x1b[0m OpenRouter — not configured"); }
    if has_local { println!("  \x1b[32m✓\x1b[0m Local models — free tier active"); }
    else { println!("  \x1b[33m-\x1b[0m Local models — none detected"); }

    println!();
    if has_ds || has_or || has_local {
        println!("  Run \x1b[1maismush-start\x1b[0m to begin coding with smart routing.");
    } else {
        println!("  Run \x1b[1maismush-start --direct\x1b[0m for Claude-only mode.");
    }
    println!("  Config saved to: {}", config_path.display());
    println!();
}

async fn setup_deepseek(client: &HttpClient, config: &mut Value) {
    let key = prompt("     Paste your DeepSeek API key: ");
    if key.is_empty() {
        println!("     \x1b[33mSkipped.\x1b[0m");
        return;
    }
    println!("     Testing connection...");
    if test_deepseek(client, &key).await {
        config["apiKey"] = Value::String(key);
    } else {
        let save = prompt("     Save anyway? [y/N]: ");
        if save.to_lowercase() == "y" {
            config["apiKey"] = Value::String(key);
        }
    }
}

async fn setup_openrouter(client: &HttpClient, config: &mut Value) {
    let key = prompt("     Paste your OpenRouter API key: ");
    if key.is_empty() {
        println!("     \x1b[33mSkipped.\x1b[0m");
        return;
    }
    println!("     Testing connection...");
    if test_openrouter(client, &key).await {
        config["openrouterKey"] = Value::String(key);
    } else {
        let save = prompt("     Save anyway? [y/N]: ");
        if save.to_lowercase() == "y" {
            config["openrouterKey"] = Value::String(key);
        }
    }
}

/// Test DeepSeek API key with a minimal completion.
async fn test_deepseek(client: &HttpClient, key: &str) -> bool {
    let body = json!({
        "model": "deepseek-chat",
        "max_tokens": 1,
        "messages": [{"role": "user", "content": "hi"}]
    });

    match api_test(client, "https://api.deepseek.com/v1/chat/completions", Some(key), &body).await {
        Ok(status) => {
            if status == 200 || status == 429 {
                println!("     \x1b[32m✓ DeepSeek key is valid!\x1b[0m");
                true
            } else if status == 401 || status == 403 {
                println!("     \x1b[31m✗ Invalid key (HTTP {})\x1b[0m", status);
                println!("       Check your key at https://platform.deepseek.com/api_keys");
                false
            } else {
                println!("     \x1b[33m? Unexpected response (HTTP {})\x1b[0m", status);
                false
            }
        }
        Err(e) => {
            println!("     \x1b[31m✗ Connection failed: {}\x1b[0m", e);
            false
        }
    }
}

/// Test OpenRouter API key with a minimal completion.
async fn test_openrouter(client: &HttpClient, key: &str) -> bool {
    let body = json!({
        "model": "deepseek/deepseek-chat",
        "max_tokens": 1,
        "messages": [{"role": "user", "content": "hi"}]
    });

    match api_test(client, "https://openrouter.ai/api/v1/chat/completions", Some(key), &body).await {
        Ok(status) => {
            if status == 200 || status == 429 {
                println!("     \x1b[32m✓ OpenRouter key is valid!\x1b[0m");
                true
            } else if status == 401 || status == 403 {
                println!("     \x1b[31m✗ Invalid key (HTTP {})\x1b[0m", status);
                println!("       Check your key at https://openrouter.ai/keys");
                false
            } else if status == 402 {
                println!("     \x1b[33m✓ Key is valid but no credits.\x1b[0m Add credits at https://openrouter.ai/credits");
                true // Key itself is valid
            } else {
                println!("     \x1b[33m? Unexpected response (HTTP {})\x1b[0m", status);
                false
            }
        }
        Err(e) => {
            println!("     \x1b[31m✗ Connection failed: {}\x1b[0m", e);
            false
        }
    }
}

/// Test a local model server with a minimal completion.
async fn test_local(client: &HttpClient, base_url: &str, model: &str) -> bool {
    let url = if base_url.contains("/v1") {
        format!("{}/chat/completions", base_url.trim_end_matches('/'))
    } else {
        format!("{}/v1/chat/completions", base_url.trim_end_matches('/'))
    };

    let body = json!({
        "model": model,
        "max_tokens": 1,
        "messages": [{"role": "user", "content": "hi"}]
    });

    match api_test(client, &url, None, &body).await {
        Ok(status) => {
            if status == 200 {
                println!("     \x1b[32m✓ {} responding correctly!\x1b[0m", model);
                true
            } else {
                println!("     \x1b[33m? {} returned HTTP {} (may still work)\x1b[0m", model, status);
                status < 500
            }
        }
        Err(e) => {
            println!("     \x1b[31m✗ Connection failed: {}\x1b[0m", e);
            false
        }
    }
}

/// Make a test API call and return the HTTP status code.
async fn api_test(client: &HttpClient, url: &str, api_key: Option<&str>, body: &Value) -> Result<u16, String> {
    let body_bytes = serde_json::to_vec(body).map_err(|e| e.to_string())?;

    let host = url.split("://").nth(1).and_then(|s| s.split('/').next()).unwrap_or("localhost");

    let mut builder = Request::builder()
        .method(hyper::Method::POST)
        .uri(url)
        .header("content-type", "application/json")
        .header("host", host);

    if let Some(key) = api_key {
        builder = builder.header("authorization", format!("Bearer {}", key));
    }

    let req = builder.body(full(Bytes::from(body_bytes))).map_err(|e| e.to_string())?;

    let result = tokio::time::timeout(
        Duration::from_secs(10),
        client.request(req),
    ).await;

    match result {
        Ok(Ok(resp)) => Ok(resp.status().as_u16()),
        Ok(Err(e)) => Err(format!("{}", e)),
        Err(_) => Err("timeout (10s)".to_string()),
    }
}

pub fn run_proxy_setup() {
    println!();
    println!("  AISmush — Proxy Pool Setup");
    println!("  ──────────────────────────");
    println!();

    let config_path = config_path();
    let mut config = load_existing_config(&config_path);

    let mut proxies: Vec<String> = config.get("proxies")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();

    if proxies.is_empty() {
        println!("  No proxies configured.");
    } else {
        println!("  Current proxies ({}):", proxies.len());
        for (i, p) in proxies.iter().enumerate() {
            println!("    {}. {}", i + 1, mask_proxy_password(p));
        }
    }
    println!();
    println!("  Supported formats:");
    println!("    host:port                  — HTTP, no auth");
    println!("    host:port:username:pass    — HTTP with Basic auth");
    println!("    socks5://host:port         — SOCKS5");
    println!();

    // Add loop
    loop {
        let s = prompt("  Add proxy (or Enter to finish): ");
        if s.is_empty() { break; }
        if !s.contains(':') {
            println!("  \x1b[31m✗ Invalid — expected at least host:port\x1b[0m");
            continue;
        }
        proxies.push(s);
        println!("  \x1b[32m✓ Added.\x1b[0m ({} total)", proxies.len());
    }

    // Remove loop
    while !proxies.is_empty() {
        println!();
        println!("  Current list ({}):", proxies.len());
        for (i, p) in proxies.iter().enumerate() {
            println!("    {}. {}", i + 1, mask_proxy_password(p));
        }
        let s = prompt("  Remove by number (or Enter to skip): ");
        if s.is_empty() { break; }
        match s.trim().parse::<usize>() {
            Ok(n) if n >= 1 && n <= proxies.len() => {
                let removed = proxies.remove(n - 1);
                println!("  \x1b[32m✓ Removed: {}\x1b[0m", mask_proxy_password(&removed));
            }
            _ => { println!("  \x1b[31mInvalid number.\x1b[0m"); break; }
        }
    }
    println!();

    // Save
    if proxies.is_empty() {
        if let Some(obj) = config.as_object_mut() { obj.remove("proxies"); }
        println!("  Proxy pool disabled (no proxies configured).");
    } else {
        config["proxies"] = serde_json::Value::Array(
            proxies.iter().map(|p| serde_json::Value::String(p.clone())).collect()
        );
        println!("  {} proxy/proxies saved.", proxies.len());
    }
    save_config(&config_path, &config);
    println!("  Restart AISmush to apply changes.");
    println!();
}

fn mask_proxy_password(proxy: &str) -> String {
    if proxy.contains("://") { return proxy.to_string(); }
    let parts: Vec<&str> = proxy.splitn(4, ':').collect();
    if parts.len() == 4 {
        format!("{}:{}:{}:****", parts[0], parts[1], parts[2])
    } else {
        proxy.to_string()
    }
}

fn prompt(msg: &str) -> String {
    print!("{}", msg);
    io::stdout().flush().ok();
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line).ok();
    line.trim().to_string()
}

fn config_path() -> PathBuf {
    let home = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".hybrid-proxy").join("config.json")
}

fn load_existing_config(path: &PathBuf) -> Value {
    if let Ok(data) = std::fs::read_to_string(path) {
        serde_json::from_str(&data).unwrap_or(json!({}))
    } else {
        json!({})
    }
}

fn save_config(path: &PathBuf, config: &Value) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    if let Ok(pretty) = serde_json::to_string_pretty(config) {
        if std::fs::write(path, pretty).is_ok() {
            println!("  \x1b[32mConfig saved.\x1b[0m");
        } else {
            eprintln!("  \x1b[31mFailed to write config.\x1b[0m");
        }
    }
}
