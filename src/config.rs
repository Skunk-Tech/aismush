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
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct FileConfig {
    api_key: Option<String>,
    port: Option<u16>,
    verbose: Option<bool>,
    force_provider: Option<String>,
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

        let db_path = data_dir.join("proxy.db");

        ProxyConfig { api_key, port, verbose, force_provider, data_dir, db_path }
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
