use reqwest::{Client, Proxy};
use std::sync::atomic::{AtomicUsize, Ordering};
use tracing::warn;

/// A single outbound proxy entry.
struct Entry {
    proxy_url: String,
    username: Option<String>,
    password: Option<String>,
}

impl Entry {
    /// Parse proxy string:
    ///   `host:port`              → HTTP proxy, no auth
    ///   `host:port:user:pass`    → HTTP proxy with Basic auth
    ///   `socks5://host:port`     → SOCKS5, no auth (full URL passed through)
    fn parse(s: &str) -> Option<Self> {
        // Full URL form (socks5://, http://, https://)
        if s.contains("://") {
            return Some(Self { proxy_url: s.to_string(), username: None, password: None });
        }

        // host:port  or  host:port:user:pass
        let parts: Vec<&str> = s.splitn(4, ':').collect();
        match parts.as_slice() {
            [host, port] => {
                let port: u16 = port.parse().ok()?;
                Some(Self {
                    proxy_url: format!("http://{}:{}", host, port),
                    username: None,
                    password: None,
                })
            }
            [host, port, user, pass] => {
                let port: u16 = port.parse().ok()?;
                Some(Self {
                    proxy_url: format!("http://{}:{}", host, port),
                    username: Some((*user).to_string()),
                    password: Some((*pass).to_string()),
                })
            }
            _ => None,
        }
    }

    fn build_client(&self) -> reqwest::Result<Client> {
        let mut proxy = Proxy::all(&self.proxy_url)?;
        if let (Some(user), Some(pass)) = (&self.username, &self.password) {
            proxy = proxy.basic_auth(user, pass);
        }
        Client::builder()
            .proxy(proxy)
            .use_rustls_tls()
            .timeout(std::time::Duration::from_secs(120))
            .build()
    }
}

/// Round-robin pool of reqwest clients, one per configured proxy.
pub struct ProxyPool {
    clients: Vec<Client>,
    idx: AtomicUsize,
}

impl ProxyPool {
    /// Build from a list of proxy strings. Returns `None` if list is empty
    /// or every entry fails to parse/build.
    pub fn build(proxy_strings: &[String]) -> Option<Self> {
        if proxy_strings.is_empty() {
            return None;
        }

        let mut clients = Vec::with_capacity(proxy_strings.len());
        for s in proxy_strings {
            match Entry::parse(s.trim()) {
                Some(entry) => match entry.build_client() {
                    Ok(c) => clients.push(c),
                    Err(e) => warn!(proxy = s, error = %e, "Failed to build proxy client, skipping"),
                },
                None => warn!(proxy = s, "Invalid proxy string, skipping"),
            }
        }

        if clients.is_empty() {
            warn!("No valid proxies configured; proxy pool disabled");
            return None;
        }

        Some(Self { clients, idx: AtomicUsize::new(0) })
    }

    /// Return the next client in round-robin order.
    pub fn next(&self) -> &Client {
        let i = self.idx.fetch_add(1, Ordering::Relaxed) % self.clients.len();
        &self.clients[i]
    }

    pub fn len(&self) -> usize {
        self.clients.len()
    }
}
