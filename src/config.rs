use serde::Deserialize;
use std::env;
use std::fs;
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct ProxyConfig {
    pub api_key: String,
    pub port: u16,
    pub verbose: bool,
    pub force_provider: Option<String>,
    pub data_dir: PathBuf,
    pub db_path: PathBuf,
    /// OpenRouter API key for access to 290+ models
    pub openrouter_api_key: String,
    /// Explicitly configured local model servers: (name, url, model)
    pub local_servers: Vec<(String, String, String)>,
    /// Auto-discover local model servers on known ports
    pub auto_discover_local: bool,
    /// Routing configuration
    pub routing: RoutingConfig,
    /// Custom tool name → category mappings for multi-client support
    pub tool_mappings: Option<crate::tools::ToolMappings>,
    /// Max concurrent in-flight requests to Claude (default 5)
    pub max_concurrent_claude: usize,
    /// Outbound proxy strings for Claude requests (round-robined)
    pub proxies: Vec<String>,
    /// Max input tokens per minute to send to Claude (0 = disabled). Default 40000 (Tier 1).
    pub max_tpm: u64,
}

#[derive(Clone, Debug)]
pub struct RoutingConfig {
    /// Blast radius score threshold for tier escalation (default 0.5)
    pub blast_radius_threshold: f64,
    /// Prefer local models when available for eligible tasks
    pub prefer_local: bool,
    /// Minimum tier for planning tasks
    pub min_tier_planning: String,
    /// Minimum tier for debugging tasks
    pub min_tier_debugging: String,
}

impl Default for RoutingConfig {
    fn default() -> Self {
        Self {
            blast_radius_threshold: 0.5,
            prefer_local: true,
            min_tier_planning: "premium".into(),
            min_tier_debugging: "mid".into(),
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct FileConfig {
    api_key: Option<String>,
    port: Option<u16>,
    verbose: Option<bool>,
    force_provider: Option<String>,
    openrouter_key: Option<String>,
    local: Option<Vec<LocalServerFileConfig>>,
    auto_discover_local: Option<bool>,
    routing: Option<RoutingFileConfig>,
    tool_mappings: Option<crate::tools::ToolMappings>,
    max_concurrent_claude: Option<usize>,
    proxies: Option<Vec<String>>,
    max_tpm: Option<u64>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct LocalServerFileConfig {
    name: String,
    url: String,
    model: String,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct RoutingFileConfig {
    blast_radius_threshold: Option<f64>,
    prefer_local: Option<bool>,
    min_tier_for_planning: Option<String>,
    min_tier_for_debugging: Option<String>,
}

impl ProxyConfig {
    pub fn load() -> Self {
        // Try loading JSON config from multiple locations
        let file_cfg = Self::load_file();

        let data_dir = dirs_home().join(".hybrid-proxy");
        fs::create_dir_all(&data_dir).ok();

        let api_key = env::var("DEEPSEEK_API_KEY")
            .ok()
            .or(file_cfg.api_key)
            .unwrap_or_default();

        let port = env::var("PROXY_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .or(file_cfg.port)
            .unwrap_or(1849);

        let verbose = env::var("PROXY_VERBOSE")
            .ok()
            .map(|v| v == "true" || v == "1")
            .or(file_cfg.verbose)
            .unwrap_or(false);

        let force_provider = env::var("FORCE_PROVIDER")
            .ok()
            .or(file_cfg.force_provider)
            .filter(|s| !s.is_empty());

        let openrouter_api_key = env::var("OPENROUTER_API_KEY")
            .ok()
            .or(file_cfg.openrouter_key)
            .unwrap_or_default();

        let mut local_servers: Vec<(String, String, String)> = Vec::new();

        // From config file
        if let Some(locals) = file_cfg.local {
            for l in locals {
                local_servers.push((l.name, l.url, l.model));
            }
        }

        // From env vars (single local server shorthand)
        if let (Ok(url), Ok(model)) = (env::var("LOCAL_MODEL_URL"), env::var("LOCAL_MODEL_NAME")) {
            if !url.is_empty() {
                let name = env::var("LOCAL_MODEL_SERVER").unwrap_or_else(|_| "local".into());
                local_servers.push((name, url, model));
            }
        }

        let auto_discover_local = env::var("AISMUSH_AUTO_DISCOVER")
            .ok()
            .map(|v| v != "false" && v != "0")
            .or(file_cfg.auto_discover_local)
            .unwrap_or(true);

        let routing = {
            let fr = file_cfg.routing.unwrap_or_default();
            let mut rc = RoutingConfig::default();
            if let Some(t) = fr.blast_radius_threshold { rc.blast_radius_threshold = t; }
            if let Some(p) = fr.prefer_local { rc.prefer_local = p; }
            if let Some(ref t) = fr.min_tier_for_planning { rc.min_tier_planning = t.clone(); }
            if let Some(ref t) = fr.min_tier_for_debugging { rc.min_tier_debugging = t.clone(); }
            // Also check env var for blast threshold (backward compat)
            if let Ok(v) = env::var("AISMUSH_BLAST_THRESHOLD") {
                if let Ok(t) = v.parse() { rc.blast_radius_threshold = t; }
            }
            rc
        };

        let db_path = data_dir.join("proxy.db");

        let tool_mappings = file_cfg.tool_mappings;

        let max_concurrent_claude = env::var("AISMUSH_MAX_CONCURRENT")
            .ok()
            .and_then(|v| v.parse().ok())
            .or(file_cfg.max_concurrent_claude)
            .unwrap_or(5);

        // AISMUSH_PROXIES=host:port,host:port:user:pass,...
        let proxies = env::var("AISMUSH_PROXIES")
            .ok()
            .map(|v| v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect::<Vec<_>>())
            .or(file_cfg.proxies)
            .unwrap_or_default();

        // AISMUSH_MAX_TPM=40000 — max input tokens/min sent to Claude (0 = disabled)
        let max_tpm = env::var("AISMUSH_MAX_TPM")
            .ok()
            .and_then(|v| v.parse().ok())
            .or(file_cfg.max_tpm)
            .unwrap_or(40_000);

        ProxyConfig { api_key, port, verbose, force_provider, data_dir, db_path, openrouter_api_key, local_servers, auto_discover_local, routing, tool_mappings, max_concurrent_claude, proxies, max_tpm }
    }

    fn load_file() -> FileConfig {
        // Try current directory first, then home
        let paths = [
            PathBuf::from(".deepseek-proxy.json"),
            PathBuf::from("config.json"),
            dirs_home().join(".hybrid-proxy").join("config.json"),
        ];

        for path in &paths {
            if let Ok(data) = fs::read_to_string(path) {
                if let Ok(cfg) = serde_json::from_str::<FileConfig>(&data) {
                    return cfg;
                }
            }
        }

        FileConfig::default()
    }
}

fn dirs_home() -> PathBuf {
    env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}
