//! Provider abstraction layer for multi-provider routing.
//!
//! Supports two API shapes: Anthropic (Claude native) and OpenAI-compatible
//! (DeepSeek, OpenRouter, Ollama, LM Studio, llama.cpp, vLLM, etc.).

use std::sync::Arc;
use std::time::Instant;

// Use std::sync::RwLock (NOT tokio::sync::RwLock) for HealthState.
// This is critical: HealthState is checked inside async functions that already
// hold the tokio registry RwLock. Using tokio RwLock for both causes deadlocks
// because tokio RwLock yields to the runtime, allowing the discovery loop to
// acquire the registry write lock while a request handler holds the read lock
// and is waiting on HealthState.
use std::sync::RwLock;

/// The two API protocol families we support.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    /// Claude's native SSE format (message_start, content_block_delta, etc.)
    Anthropic,
    /// OpenAI-compatible /v1/chat/completions (used by everything else)
    OpenAICompat,
}

/// Capability/cost tiers, ordered from cheapest to most expensive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Tier {
    Free,    // Local models — zero cost
    Budget,  // Small/cheap cloud models
    Mid,     // DeepSeek, GPT-4o-mini, Llama 405B
    Premium, // Claude Sonnet, GPT-4o
    Ultra,   // Claude Opus
}

impl Tier {
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "free" => Tier::Free,
            "budget" => Tier::Budget,
            "mid" => Tier::Mid,
            "premium" => Tier::Premium,
            "ultra" => Tier::Ultra,
            _ => Tier::Mid,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Tier::Free => "free",
            Tier::Budget => "budget",
            Tier::Mid => "mid",
            Tier::Premium => "premium",
            Tier::Ultra => "ultra",
        }
    }
}

impl std::fmt::Display for Tier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Health state of a provider, updated by health checks and request outcomes.
#[derive(Debug, Clone)]
pub enum HealthState {
    Healthy,
    Degraded { error_count: usize, last_error: Instant },
    RateLimited { until: Instant, retry_after_secs: u64 },
    Down { since: Instant, last_check: Instant },
}

impl Default for HealthState {
    fn default() -> Self {
        HealthState::Healthy
    }
}

impl HealthState {
    pub fn is_healthy(&self) -> bool {
        match self {
            HealthState::Healthy | HealthState::Degraded { .. } => true,
            HealthState::RateLimited { until, .. } => Instant::now() >= *until,
            // Circuit-breaker half-open: after 10 minutes down, let one request through.
            // If it succeeds, mark_healthy() is called and the provider recovers.
            HealthState::Down { since, .. } => since.elapsed().as_secs() > 600,
        }
    }

    pub fn is_down(&self) -> bool {
        // Only report as fully down if within the 10-minute circuit-breaker window.
        matches!(self, HealthState::Down { since, .. } if since.elapsed().as_secs() <= 600)
    }

    pub fn is_rate_limited(&self) -> bool {
        matches!(self, HealthState::RateLimited { until, .. } if Instant::now() < *until)
    }
}

/// Configuration for a single AI provider.
#[derive(Debug)]
pub struct ProviderConfig {
    /// Unique identifier: "claude", "deepseek", "openrouter-gpt4o", "ollama-qwen3"
    pub id: String,
    /// API protocol family
    pub kind: ProviderKind,
    /// Base URL for API requests (e.g. "https://api.anthropic.com", "http://localhost:11434")
    pub base_url: String,
    /// API key (None for local models that don't require auth)
    pub api_key: Option<String>,
    /// Default model name to use
    pub default_model: String,
    /// Context window size in tokens
    pub context_window: usize,
    /// Capability/cost tier
    pub tier: Tier,
    /// Pricing: (input_per_million, output_per_million) in USD
    pub pricing: (f64, f64),
    /// Maximum output tokens the model supports
    pub max_output_tokens: usize,
    /// Whether the model reliably handles tool_use/function_calling
    pub supports_tools: bool,
    /// Whether this provider supports streaming
    pub supports_streaming: bool,
    /// Current health state
    pub health: RwLock<HealthState>,
    /// Whether this was auto-discovered (vs explicitly configured)
    pub auto_discovered: bool,
}

impl ProviderConfig {
    pub fn new(
        id: impl Into<String>,
        kind: ProviderKind,
        base_url: impl Into<String>,
        api_key: Option<String>,
        default_model: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            kind,
            base_url: base_url.into(),
            api_key,
            default_model: default_model.into(),
            context_window: 128_000,
            tier: Tier::Mid,
            pricing: (0.0, 0.0),
            max_output_tokens: 8192,
            supports_tools: true,
            supports_streaming: true,
            health: RwLock::new(HealthState::Healthy),
            auto_discovered: false,
        }
    }

    pub fn with_tier(mut self, tier: Tier) -> Self {
        self.tier = tier;
        self
    }

    pub fn with_pricing(mut self, input: f64, output: f64) -> Self {
        self.pricing = (input, output);
        self
    }

    pub fn with_context_window(mut self, tokens: usize) -> Self {
        self.context_window = tokens;
        self
    }

    pub fn with_max_output(mut self, tokens: usize) -> Self {
        self.max_output_tokens = tokens;
        self
    }

    pub fn with_tools(mut self, supports: bool) -> Self {
        self.supports_tools = supports;
        self
    }

    pub fn with_auto_discovered(mut self, auto: bool) -> Self {
        self.auto_discovered = auto;
        self
    }

    pub fn is_healthy(&self) -> bool {
        self.health.read().unwrap().is_healthy()
    }

    pub fn mark_healthy(&self) {
        *self.health.write().unwrap() = HealthState::Healthy;
    }

    pub fn mark_error(&self) {
        let mut health = self.health.write().unwrap();
        match &*health {
            HealthState::Healthy => {
                *health = HealthState::Degraded {
                    error_count: 1,
                    last_error: Instant::now(),
                };
            }
            HealthState::Degraded { error_count, .. } => {
                let count = *error_count + 1;
                if count >= 3 {
                    *health = HealthState::Down {
                        since: Instant::now(),
                        last_check: Instant::now(),
                    };
                } else {
                    *health = HealthState::Degraded {
                        error_count: count,
                        last_error: Instant::now(),
                    };
                }
            }
            HealthState::RateLimited { .. } => {
                // Don't override rate-limit with generic error
            }
            HealthState::Down { since, .. } => {
                *health = HealthState::Down {
                    since: *since,
                    last_check: Instant::now(),
                };
            }
        }
    }

    pub fn mark_rate_limited(&self, retry_after_secs: u64) {
        *self.health.write().unwrap() = HealthState::RateLimited {
            until: Instant::now() + std::time::Duration::from_secs(retry_after_secs),
            retry_after_secs,
        };
    }

    pub fn is_rate_limited(&self) -> bool {
        self.health.read().unwrap().is_rate_limited()
    }

    pub fn mark_down(&self) {
        *self.health.write().unwrap() = HealthState::Down {
            since: Instant::now(),
            last_check: Instant::now(),
        };
    }
}

/// Registry of all available providers, used by the router to select targets.
pub struct ProviderRegistry {
    pub providers: Vec<Arc<ProviderConfig>>,
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self { providers: Vec::new() }
    }

    pub fn register(&mut self, provider: ProviderConfig) {
        self.providers.push(Arc::new(provider));
    }

    /// Get a provider by ID.
    pub fn get(&self, id: &str) -> Option<Arc<ProviderConfig>> {
        self.providers.iter().find(|p| p.id == id).cloned()
    }

    /// Get all healthy providers at a given tier or above.
    pub async fn at_or_above_tier(&self, min_tier: Tier) -> Vec<Arc<ProviderConfig>> {
        let mut result = Vec::new();
        for p in &self.providers {
            if p.tier >= min_tier && p.is_healthy() {
                result.push(p.clone());
            }
        }
        result
    }

    /// Get the cheapest healthy provider at or above a minimum tier,
    /// optionally filtering by context window and tool support.
    pub async fn cheapest_healthy(
        &self,
        min_tier: Tier,
        min_context: usize,
        needs_tools: bool,
    ) -> Option<Arc<ProviderConfig>> {
        let mut candidates = Vec::new();
        for p in &self.providers {
            if p.tier >= min_tier
                && p.context_window >= min_context
                && (!needs_tools || p.supports_tools)
                && p.is_healthy()
            {
                candidates.push(p.clone());
            }
        }
        // Sort by tier first (prefer lower/cheaper), then by input pricing
        candidates.sort_by(|a, b| {
            a.tier.cmp(&b.tier)
                .then(a.pricing.0.partial_cmp(&b.pricing.0).unwrap_or(std::cmp::Ordering::Equal))
        });
        candidates.into_iter().next()
    }

    /// Build a fallback chain: starting from the given provider, list alternatives
    /// in order of increasing cost, filtered by context window.
    pub async fn fallback_chain(
        &self,
        exclude_id: &str,
        min_context: usize,
        needs_tools: bool,
    ) -> Vec<Arc<ProviderConfig>> {
        let mut chain = Vec::new();
        for p in &self.providers {
            if p.id != exclude_id
                && p.context_window >= min_context
                && (!needs_tools || p.supports_tools)
                && p.is_healthy()
            {
                chain.push(p.clone());
            }
        }
        chain.sort_by(|a, b| {
            a.tier.cmp(&b.tier)
                .then(a.pricing.0.partial_cmp(&b.pricing.0).unwrap_or(std::cmp::Ordering::Equal))
        });
        chain
    }

    /// Check if any provider with a given ID exists (healthy or not).
    pub fn has_provider(&self, id: &str) -> bool {
        self.providers.iter().any(|p| p.id == id)
    }

    /// Remove auto-discovered providers that are no longer responding.
    pub fn remove_stale_discovered(&mut self, id: &str) {
        self.providers.retain(|p| !(p.id == id && p.auto_discovered));
    }

    /// List all providers with their current status (for --providers CLI flag).
    pub async fn status_report(&self) -> Vec<ProviderStatus> {
        let mut report = Vec::new();
        for p in &self.providers {
            let health = p.health.read().unwrap();
            report.push(ProviderStatus {
                id: p.id.clone(),
                kind: p.kind,
                tier: p.tier,
                model: p.default_model.clone(),
                base_url: p.base_url.clone(),
                context_window: p.context_window,
                pricing: p.pricing,
                supports_tools: p.supports_tools,
                auto_discovered: p.auto_discovered,
                healthy: health.is_healthy(),
                down: health.is_down(),
            });
        }
        report
    }
}

/// Snapshot of a provider's status for display.
#[derive(Debug)]
pub struct ProviderStatus {
    pub id: String,
    pub kind: ProviderKind,
    pub tier: Tier,
    pub model: String,
    pub base_url: String,
    pub context_window: usize,
    pub pricing: (f64, f64),
    pub supports_tools: bool,
    pub auto_discovered: bool,
    pub healthy: bool,
    pub down: bool,
}

/// Build the default provider registry from configuration.
pub fn build_registry(
    deepseek_api_key: &str,
    openrouter_api_key: &str,
    glm_api_key: &str,
    glm_coding: bool,
    local_servers: &[(String, String, String)],          // (name, url, model)
    litellm_servers: &[(String, String, String, String)], // (name, url, model, key)
) -> ProviderRegistry {
    let mut registry = ProviderRegistry::new();

    // Claude is always available (API key comes from request headers, not our config)
    registry.register(
        ProviderConfig::new("claude", ProviderKind::Anthropic, "https://api.anthropic.com", None, "claude-sonnet-4-20250514")
            .with_tier(Tier::Premium)
            .with_pricing(3.0, 15.0)
            .with_context_window(200_000)
            .with_max_output(16384)
    );

    // DeepSeek if key is configured
    if !deepseek_api_key.is_empty() {
        registry.register(
            ProviderConfig::new("deepseek", ProviderKind::OpenAICompat, "https://api.deepseek.com", Some(deepseek_api_key.to_string()), "deepseek-chat")
                .with_tier(Tier::Mid)
                .with_pricing(0.27, 1.10)
                .with_context_window(64_000)
                .with_max_output(16384)
        );
    }

    // OpenRouter if key is configured
    if !openrouter_api_key.is_empty() {
        // Register a few commonly-used OpenRouter model tiers
        registry.register(
            ProviderConfig::new("openrouter", ProviderKind::OpenAICompat, "https://openrouter.ai/api", Some(openrouter_api_key.to_string()), "deepseek/deepseek-chat")
                .with_tier(Tier::Mid)
                .with_pricing(0.27, 1.10)
                .with_context_window(64_000)
                .with_max_output(16384)
        );
    }

    // GLM (Zhipu AI) if key is configured
    if !glm_api_key.is_empty() {
        let (base_url, model) = if glm_coding {
            ("https://api.z.ai/api/coding/paas/v4", "codegeex-4")
        } else {
            ("https://api.z.ai/api/paas/v4", "glm-4-plus")
        };
        registry.register(
            ProviderConfig::new("glm", ProviderKind::OpenAICompat, base_url, Some(glm_api_key.to_string()), model)
                .with_tier(Tier::Mid)
                .with_pricing(0.14, 0.14) // glm-4-plus pricing (¥1/M tokens ≈ $0.14/M)
                .with_context_window(128_000)
                .with_max_output(4096)
        );
    }

    // LiteLLM proxy endpoints (public or private, OpenAI-compatible)
    for (name, url, model, key) in litellm_servers {
        let id = format!("litellm-{}", name);
        let api_key = if key.is_empty() { None } else { Some(key.to_string()) };
        registry.register(
            ProviderConfig::new(id, ProviderKind::OpenAICompat, url.clone(), api_key, model.clone())
                .with_tier(Tier::Mid)
                .with_pricing(0.0, 0.0) // unknown — treat as cost-neutral
                .with_context_window(128_000)
                .with_max_output(16384)
                .with_tools(true)
        );
    }

    // Explicitly configured local servers
    for (name, url, model) in local_servers {
        let id = format!("local-{}", name);
        registry.register(
            ProviderConfig::new(id, ProviderKind::OpenAICompat, url.clone(), None, model.clone())
                .with_tier(Tier::Free)
                .with_pricing(0.0, 0.0)
                .with_context_window(32_000) // conservative default, updated by discovery
                .with_max_output(4096)
                .with_tools(false) // conservative default
        );
    }

    registry
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tier_ordering() {
        assert!(Tier::Free < Tier::Budget);
        assert!(Tier::Budget < Tier::Mid);
        assert!(Tier::Mid < Tier::Premium);
        assert!(Tier::Premium < Tier::Ultra);
    }

    #[test]
    fn test_tier_from_str() {
        assert_eq!(Tier::from_str("free"), Tier::Free);
        assert_eq!(Tier::from_str("Premium"), Tier::Premium);
        assert_eq!(Tier::from_str("unknown"), Tier::Mid);
    }

    #[tokio::test]
    async fn test_registry_cheapest() {
        let registry = build_registry("sk-test", "", "", false, &[], &[]);
        // Should find DeepSeek as cheapest at Mid tier
        let cheapest = registry.cheapest_healthy(Tier::Mid, 0, false).await;
        assert!(cheapest.is_some());
        assert_eq!(cheapest.unwrap().id, "deepseek");
    }

    #[tokio::test]
    async fn test_registry_premium_gets_claude() {
        let registry = build_registry("sk-test", "", "", false, &[], &[]);
        let cheapest = registry.cheapest_healthy(Tier::Premium, 0, false).await;
        assert!(cheapest.is_some());
        assert_eq!(cheapest.unwrap().id, "claude");
    }

    #[tokio::test]
    async fn test_fallback_chain() {
        let registry = build_registry("sk-test", "", "", false, &[], &[]);
        let chain = registry.fallback_chain("deepseek", 0, false).await;
        assert!(!chain.is_empty());
        assert_eq!(chain[0].id, "claude"); // Claude is the fallback
    }

    #[tokio::test]
    async fn test_health_state_transitions() {
        let provider = ProviderConfig::new("test", ProviderKind::OpenAICompat, "http://localhost", None, "test-model");
        assert!(provider.is_healthy());

        provider.mark_error();
        assert!(provider.is_healthy()); // still healthy after 1 error (degraded)

        provider.mark_error();
        assert!(provider.is_healthy()); // still degraded after 2

        provider.mark_error();
        assert!(!provider.is_healthy()); // down after 3 errors

        provider.mark_healthy();
        assert!(provider.is_healthy()); // recovered
    }
}
