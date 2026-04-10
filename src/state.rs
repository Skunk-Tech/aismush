use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use hyper_util::client::legacy::Client;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::sync::RwLock;

use std::collections::HashMap;
use crate::config::ProxyConfig;
use crate::db::Db;
use crate::embeddings::EmbeddingEngine;
use crate::file_cache::FileCache;
use crate::provider::ProviderRegistry;
use crate::tools::ToolClassifier;

pub type HttpClient = Client<
    hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>,
    BoxBody<Bytes, hyper::Error>,
>;

pub struct ProxyState {
    pub config: ProxyConfig,
    pub client: HttpClient,
    pub stats: Mutex<Stats>,
    pub last_reported: Mutex<ReportedStats>,
    pub db: Option<Db>,
    pub embedder: RwLock<Option<EmbeddingEngine>>,
    pub dashboard_html: String,
    pub instance_id: String,
    pub registry: RwLock<ProviderRegistry>,
    pub file_cache: Mutex<FileCache>,
    /// Timestamp (epoch secs) when last 429 was received. If within 60s, memory injection is skipped.
    pub rate_limit_pressure: std::sync::atomic::AtomicU64,
    /// Limits concurrent in-flight requests to Claude to avoid 429s.
    pub claude_semaphore: Arc<tokio::sync::Semaphore>,
    /// Round-robin pool of outbound proxy clients for Claude requests.
    pub proxy_pool: Option<crate::proxy_pool::ProxyPool>,
    /// Tool classifier for client-agnostic tool name detection.
    /// Used by intelligence pipeline; will be passed to file_cache/memory/capture when refactored.
    #[allow(dead_code)]
    pub tool_classifier: ToolClassifier,
}

#[derive(Default, Debug, serde::Serialize)]
pub struct Stats {
    pub total_requests: u64,
    pub claude_turns: u64,
    pub deepseek_turns: u64,
    pub claude_bytes: u64,
    pub deepseek_bytes: u64,
    pub compressed_original_bytes: u64,
    pub compressed_final_bytes: u64,
    pub tool_results_compressed: u64,
    pub estimated_tokens_routed: u64,
    /// Dynamic per-provider turn counts (includes all providers)
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub provider_turns: HashMap<String, u64>,
    /// Dynamic per-provider byte counts
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub provider_bytes: HashMap<String, u64>,
}

/// Tracks the last values sent to the global dashboard so we can compute deltas.
#[derive(Default, Debug)]
pub struct ReportedStats {
    pub requests: i64,
    pub claude_turns: i64,
    pub deepseek_turns: i64,
    pub routing_savings: f64,
    pub compression_savings: f64,
    pub compressed_tokens: i64,
}

impl ProxyState {
    pub fn new(config: ProxyConfig, client: HttpClient, db: Option<Db>, embedder: Option<EmbeddingEngine>, dashboard_html: String, registry: ProviderRegistry) -> Arc<Self> {
        let instance_id = load_or_create_instance_id(&config.data_dir);
        let tool_classifier = ToolClassifier::new(config.tool_mappings.as_ref());
        let max_concurrent = config.max_concurrent_claude;
        let proxy_pool = crate::proxy_pool::ProxyPool::build(&config.proxies);
        Arc::new(Self {
            config,
            client,
            stats: Mutex::new(Stats::default()),
            last_reported: Mutex::new(ReportedStats::default()),
            db,
            embedder: RwLock::new(embedder),
            dashboard_html,
            instance_id,
            registry: RwLock::new(registry),
            file_cache: Mutex::new(FileCache::new(500)),
            rate_limit_pressure: std::sync::atomic::AtomicU64::new(0),
            claude_semaphore: Arc::new(tokio::sync::Semaphore::new(max_concurrent)),
            proxy_pool,
            tool_classifier,
        })
    }
}

/// Persistent machine fingerprint. Created once, stored in data_dir/instance_id.
/// Survives restarts so the global dashboard can count unique installations.
fn load_or_create_instance_id(data_dir: &std::path::Path) -> String {
    let id_path = data_dir.join("instance_id");
    if let Ok(id) = std::fs::read_to_string(&id_path) {
        let id = id.trim().to_string();
        if !id.is_empty() {
            return id;
        }
    }
    // Generate a new persistent ID from random bytes
    let mut buf = [0u8; 16];
    if let Ok(f) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        let mut f = f;
        let _ = f.read_exact(&mut buf);
    } else {
        // Fallback: hash data_dir + PID + timestamp
        use ring::digest;
        use std::time::{SystemTime, UNIX_EPOCH};
        let seed = format!(
            "{}-{}-{}",
            std::process::id(),
            data_dir.display(),
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos(),
        );
        let d = digest::digest(&digest::SHA256, seed.as_bytes());
        buf.copy_from_slice(&d.as_ref()[..16]);
    }
    let id = buf.iter().map(|b| format!("{:02x}", b)).collect::<String>();
    std::fs::write(&id_path, &id).ok();
    id
}
