use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use hyper_util::client::legacy::Client;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::sync::RwLock;

use crate::config::ProxyConfig;
use crate::db::Db;
use crate::embeddings::EmbeddingEngine;

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
    pub fn new(config: ProxyConfig, client: HttpClient, db: Option<Db>, embedder: Option<EmbeddingEngine>, dashboard_html: String) -> Arc<Self> {
        let instance_id = load_or_create_instance_id(&config.data_dir);
        Arc::new(Self {
            config,
            client,
            stats: Mutex::new(Stats::default()),
            last_reported: Mutex::new(ReportedStats::default()),
            db,
            embedder: RwLock::new(embedder),
            dashboard_html,
            instance_id,
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
