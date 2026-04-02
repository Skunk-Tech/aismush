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
    pub db: Option<Db>,
    pub embedder: RwLock<Option<EmbeddingEngine>>,
    pub dashboard_html: String,
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
    pub fn new(config: ProxyConfig, client: HttpClient, db: Option<Db>, embedder: Option<EmbeddingEngine>, dashboard_html: String) -> Arc<Self> {
        Arc::new(Self {
            config,
            client,
            stats: Mutex::new(Stats::default()),
            db,
            embedder: RwLock::new(embedder),
            dashboard_html,
        })
    }
}
