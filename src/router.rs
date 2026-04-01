use serde_json::Value;
use crate::tokens;

/// Routing decision result.
pub struct RouteDecision {
    pub provider: &'static str,
    pub reason: &'static str,
    pub estimated_tokens: usize,
}

/// Decide whether a request should go to Claude or DeepSeek.
/// Uses heuristics + context size awareness.
pub fn decide(body: &Option<Value>, force_provider: &Option<String>) -> RouteDecision {
    // Forced provider overrides everything
    if let Some(forced) = force_provider {
        return RouteDecision {
            provider: if forced == "deepseek" { "deepseek" } else { "claude" },
            reason: "forced",
            estimated_tokens: body.as_ref().map(|b| tokens::estimate_tokens(b)).unwrap_or(0),
        };
    }

    let Some(b) = body else {
        return RouteDecision { provider: "claude", reason: "no-body", estimated_tokens: 0 };
    };

    let estimated_tokens = tokens::estimate_tokens(b);

    // If the request explicitly asks for deepseek model, route there
    if let Some(model) = b["model"].as_str() {
        if model.starts_with("deepseek") {
            return RouteDecision { provider: "deepseek", reason: "model-requested", estimated_tokens };
        }
    }

    let Some(msgs) = b["messages"].as_array() else {
        return RouteDecision { provider: "claude", reason: "no-messages", estimated_tokens };
    };

    let last = msgs.last();

    // Check for tool history
    let has_tools = msgs.iter().any(|m| {
        m["content"].as_array().map_or(false, |a| {
            a.iter().any(|c| matches!(c["type"].as_str(), Some("tool_use" | "tool_result")))
        })
    });

    // Is the last message purely tool results?
    let is_tool_result = last.map_or(false, |m| {
        m["role"].as_str() == Some("user")
            && m["content"].as_array().map_or(false, |a| {
                !a.is_empty() && a.iter().all(|c| c["type"].as_str() == Some("tool_result"))
            })
    });

    // Count recent errors (signals debugging — prefer Claude)
    let recent_errors = msgs.iter().rev().take(6).filter(|m| {
        m["content"].as_array().map_or(false, |a| {
            a.iter().any(|c| c.get("is_error").and_then(|e| e.as_bool()).unwrap_or(false))
        })
    }).count();

    // Note: context size routing is now handled by context.rs (prepare/ensure_fits)
    // Don't force Claude here — let context.rs handle it with proper fallback

    // ── Fresh session (≤2 messages, no tools) → Claude ─────────────────
    if msgs.len() <= 2 && !has_tools {
        return RouteDecision { provider: "claude", reason: "initial-planning", estimated_tokens };
    }

    // ── Error recovery (3+ recent errors) → Claude ─────────────────────
    if recent_errors >= 3 {
        return RouteDecision { provider: "claude", reason: "error-recovery", estimated_tokens };
    }

    // ── Tool result turn → DeepSeek ────────────────────────────────────
    if is_tool_result {
        return RouteDecision { provider: "deepseek", reason: "tool-result", estimated_tokens };
    }

    // ── Mid-session with tool history → DeepSeek ───────────────────────
    if has_tools {
        return RouteDecision { provider: "deepseek", reason: "mid-session", estimated_tokens };
    }

    // ── Default → Claude ───────────────────────────────────────────────
    RouteDecision { provider: "claude", reason: "default", estimated_tokens }
}
