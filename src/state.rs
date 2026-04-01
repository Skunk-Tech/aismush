use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use hyper_util::client::legacy::Client;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::config::ProxyConfig;
use crate::db::Db;

pub type HttpClient = Client<
    hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>,
    BoxBody<Bytes, hyper::Error>,
>;

pub struct ProxyState {
    pub config: ProxyConfig,
    pub client: HttpClient,
    pub stats: Mutex<Stats>,
    pub db: Option<Db>,
    /// Serialize API requests to prevent concurrent tool use issues
    pub request_lock: tokio::sync::Semaphore,
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

impl ProxyState {
    pub fn new(config: ProxyConfig, client: HttpClient, db: Option<Db>) -> Arc<Self> {
        Arc::new(Self {
            config,
            client,
            stats: Mutex::new(Stats::default()),
            db,
            request_lock: tokio::sync::Semaphore::new(1), // One request at a time
        })
    }
}
