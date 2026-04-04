use serde_json::Value;
use crate::provider::{ProviderConfig, ProviderRegistry, Tier};
use crate::tokens;
use std::sync::Arc;

/// Classification of what kind of work this turn requires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskType {
    /// Initial planning, architecture, design (first messages)
    Planning,
    /// Active debugging with errors present
    Debugging,
    /// Processing tool results (file reads, command output)
    ToolResult,
    /// Simple code edits, small changes
    SimpleEdit,
    /// Reading/exploring files
    FileRead,
    /// Generating substantial new code
    CodeGeneration,
    /// Default / unclassified
    Default,
}

impl TaskType {
    pub fn as_str(&self) -> &'static str {
        match self {
            TaskType::Planning => "planning",
            TaskType::Debugging => "debugging",
            TaskType::ToolResult => "tool-result",
            TaskType::SimpleEdit => "simple-edit",
            TaskType::FileRead => "file-read",
            TaskType::CodeGeneration => "code-generation",
            TaskType::Default => "default",
        }
    }
}

/// All signals that feed into the routing decision.
pub struct RoutingSignals {
    pub blast_radius_score: Option<f64>,
    pub task_type: TaskType,
    pub recent_error_count: usize,
    pub message_count: usize,
    pub has_tool_history: bool,
    pub is_tool_result_turn: bool,
    pub estimated_tokens: usize,
    pub requires_tool_support: bool,
}

/// The routing decision — which provider to use and why.
pub struct RouteDecision {
    pub provider: String,
    pub reason: String,
    pub estimated_tokens: usize,
    pub tier: Tier,
    /// If set, the chosen provider config to use for forwarding
    pub provider_config: Option<Arc<ProviderConfig>>,
}

/// Legacy decide function — used when registry is not available or for forced providers.
/// Maintained for backward compatibility with existing call sites.
pub fn decide(body: &Option<Value>, force_provider: &Option<String>) -> RouteDecision {
    if let Some(forced) = force_provider {
        return RouteDecision {
            provider: if forced == "deepseek" { "deepseek".into() } else { "claude".into() },
            reason: "forced".into(),
            estimated_tokens: body.as_ref().map(|b| tokens::estimate_tokens(b)).unwrap_or(0),
            tier: if forced == "deepseek" { Tier::Mid } else { Tier::Premium },
            provider_config: None,
        };
    }

    let Some(b) = body else {
        return RouteDecision { provider: "claude".into(), reason: "no-body".into(), estimated_tokens: 0, tier: Tier::Premium, provider_config: None };
    };

    let signals = gather_signals(b, None);
    let min_tier = determine_min_tier(&signals, 0.5);

    // Without registry, fall back to simple Claude/DeepSeek decision
    let (provider, tier) = if min_tier >= Tier::Premium {
        ("claude", Tier::Premium)
    } else {
        ("deepseek", Tier::Mid)
    };

    RouteDecision {
        provider: provider.into(),
        reason: signals.task_type.as_str().into(),
        estimated_tokens: signals.estimated_tokens,
        tier,
        provider_config: None,
    }
}

/// Full multi-factor routing decision using the provider registry.
pub async fn decide_with_registry(
    body: &Option<Value>,
    force_provider: &Option<String>,
    blast_radius_score: Option<f64>,
    blast_threshold: f64,
    registry: &ProviderRegistry,
) -> RouteDecision {
    // Forced provider overrides everything
    if let Some(forced) = force_provider {
        let estimated = body.as_ref().map(|b| tokens::estimate_tokens(b)).unwrap_or(0);
        if let Some(provider) = registry.get(forced) {
            return RouteDecision {
                provider: provider.id.clone(),
                reason: "forced".into(),
                estimated_tokens: estimated,
                tier: provider.tier,
                provider_config: Some(provider),
            };
        }
        // Fallback: treat as claude/deepseek
        return RouteDecision {
            provider: if forced == "deepseek" { "deepseek".into() } else { "claude".into() },
            reason: "forced".into(),
            estimated_tokens: estimated,
            tier: if forced == "deepseek" { Tier::Mid } else { Tier::Premium },
            provider_config: None,
        };
    }

    let Some(b) = body else {
        let provider = registry.get("claude");
        return RouteDecision {
            provider: "claude".into(), reason: "no-body".into(), estimated_tokens: 0, tier: Tier::Premium,
            provider_config: provider,
        };
    };

    // If request explicitly asks for a model by name
    if let Some(model) = b["model"].as_str() {
        if model.starts_with("deepseek") {
            let provider = registry.get("deepseek");
            return RouteDecision {
                provider: "deepseek".into(),
                reason: "model-requested".into(),
                estimated_tokens: tokens::estimate_tokens(b),
                tier: Tier::Mid,
                provider_config: provider,
            };
        }
    }

    let signals = gather_signals(b, blast_radius_score);

    // Step 1: Determine minimum required tier
    let min_tier = determine_min_tier(&signals, blast_threshold);

    // Step 2: Check if request needs tool support
    let needs_tools = signals.requires_tool_support;

    // Step 3: Find cheapest healthy provider at the required tier
    let provider = registry.cheapest_healthy(
        min_tier,
        signals.estimated_tokens,
        needs_tools,
    ).await;

    match provider {
        Some(p) => {
            let reason = format_reason(&signals, min_tier);
            RouteDecision {
                provider: p.id.clone(),
                reason,
                estimated_tokens: signals.estimated_tokens,
                tier: p.tier,
                provider_config: Some(p),
            }
        }
        None => {
            // No provider at required tier — fall back to Claude (always available)
            let claude = registry.get("claude");
            RouteDecision {
                provider: "claude".into(),
                reason: format!("{}/fallback", signals.task_type.as_str()),
                estimated_tokens: signals.estimated_tokens,
                tier: Tier::Premium,
                provider_config: claude,
            }
        }
    }
}

/// Gather all routing signals from the request body.
pub fn gather_signals(body: &Value, blast_radius_score: Option<f64>) -> RoutingSignals {
    let estimated_tokens = tokens::estimate_tokens(body);
    let msgs = body["messages"].as_array();

    let message_count = msgs.map(|m| m.len()).unwrap_or(0);
    let last = msgs.and_then(|m| m.last());

    // Check for tool history
    let has_tools = msgs.map_or(false, |ms| {
        ms.iter().any(|m| {
            m["content"].as_array().map_or(false, |a| {
                a.iter().any(|c| matches!(c["type"].as_str(), Some("tool_use" | "tool_result")))
            })
        })
    });

    // Is the last message purely tool results?
    let is_tool_result = last.map_or(false, |m| {
        m["role"].as_str() == Some("user")
            && m["content"].as_array().map_or(false, |a| {
                !a.is_empty() && a.iter().all(|c| c["type"].as_str() == Some("tool_result"))
            })
    });

    // Count recent errors
    let recent_errors = msgs.map_or(0, |ms| {
        ms.iter().rev().take(6).filter(|m| {
            m["content"].as_array().map_or(false, |a| {
                a.iter().any(|c| c.get("is_error").and_then(|e| e.as_bool()).unwrap_or(false))
            })
        }).count()
    });

    // Check if tools are defined in the request (model needs tool support)
    let requires_tool_support = body.get("tools").and_then(|t| t.as_array()).map_or(false, |a| !a.is_empty());

    // Classify task type
    let task_type = classify_task(body, message_count, has_tools, is_tool_result, recent_errors);

    RoutingSignals {
        blast_radius_score,
        task_type,
        recent_error_count: recent_errors,
        message_count,
        has_tool_history: has_tools,
        is_tool_result_turn: is_tool_result,
        estimated_tokens,
        requires_tool_support,
    }
}

/// Classify the task type based on message analysis.
fn classify_task(
    body: &Value,
    message_count: usize,
    has_tools: bool,
    is_tool_result: bool,
    recent_errors: usize,
) -> TaskType {
    // Pure tool result turn — mechanical processing
    if is_tool_result {
        return TaskType::ToolResult;
    }

    // Error recovery
    if recent_errors >= 3 {
        return TaskType::Debugging;
    }

    // Fresh session — planning
    if message_count <= 2 && !has_tools {
        return TaskType::Planning;
    }

    // Check user text for task hints
    if let Some(msgs) = body["messages"].as_array() {
        if let Some(last) = msgs.last() {
            let text = extract_text(last);
            let lower = text.to_lowercase();

            // Planning keywords
            if lower.contains("plan") || lower.contains("architect") || lower.contains("design")
                || lower.contains("refactor") || lower.contains("restructure")
                || lower.contains("how should") || lower.contains("what's the best way") {
                return TaskType::Planning;
            }

            // Debugging keywords
            if lower.contains("fix") || lower.contains("error") || lower.contains("bug")
                || lower.contains("debug") || lower.contains("failing") || lower.contains("broken")
                || lower.contains("crash") || lower.contains("panic") {
                return TaskType::Debugging;
            }

            // File read patterns (tool results containing file content)
            if lower.contains("read") || lower.contains("show me") || lower.contains("what's in") {
                return TaskType::FileRead;
            }
        }
    }

    // Mid-session with tool history — likely code generation/execution
    if has_tools {
        return TaskType::CodeGeneration;
    }

    TaskType::Default
}

/// Determine the minimum tier required based on all routing signals.
fn determine_min_tier(signals: &RoutingSignals, blast_threshold: f64) -> Tier {
    let mut min_tier = Tier::Free;

    // Task type requirements
    let task_tier = match signals.task_type {
        TaskType::Planning => Tier::Premium,
        TaskType::Debugging => Tier::Mid,
        TaskType::ToolResult => Tier::Free,
        TaskType::FileRead => Tier::Free,
        TaskType::SimpleEdit => Tier::Budget,
        TaskType::CodeGeneration => Tier::Mid,
        TaskType::Default => Tier::Mid,
    };
    if task_tier > min_tier { min_tier = task_tier; }

    // Error escalation overrides everything
    if signals.recent_error_count >= 3 {
        if Tier::Premium > min_tier { min_tier = Tier::Premium; }
    }

    // Blast radius escalation
    if let Some(score) = signals.blast_radius_score {
        let blast_tier = if score > blast_threshold + 0.2 {
            Tier::Premium  // High blast radius — use premium model
        } else if score > blast_threshold {
            Tier::Mid      // Moderate blast radius
        } else {
            Tier::Free     // Low blast radius — anything goes
        };
        if blast_tier > min_tier { min_tier = blast_tier; }
    }

    // Large context might need a premium provider
    if signals.estimated_tokens > 100_000 {
        if Tier::Premium > min_tier { min_tier = Tier::Premium; }
    } else if signals.estimated_tokens > 50_000 {
        if Tier::Mid > min_tier { min_tier = Tier::Mid; }
    }

    min_tier
}

/// Format a human-readable reason string for the routing decision.
fn format_reason(signals: &RoutingSignals, tier: Tier) -> String {
    let task = signals.task_type.as_str();
    if let Some(score) = signals.blast_radius_score {
        if score > 0.5 {
            return format!("{}/blast-radius-{:.1}", task, score);
        }
    }
    if signals.recent_error_count >= 3 {
        return format!("{}/error-recovery", task);
    }
    task.to_string()
}

/// Extract text content from a message.
fn extract_text(msg: &Value) -> String {
    match msg.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => {
            blocks.iter()
                .filter_map(|b| {
                    if b["type"].as_str() == Some("text") {
                        b["text"].as_str().map(|s| s.to_string())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join(" ")
        }
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_classify_planning() {
        let body = json!({
            "messages": [
                {"role": "user", "content": "Plan the authentication system for our API"},
            ]
        });
        let task = classify_task(&body, 1, false, false, 0);
        assert_eq!(task, TaskType::Planning);
    }

    #[test]
    fn test_classify_tool_result() {
        let body = json!({
            "messages": [
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "call_1", "content": "file contents"},
                ]},
            ]
        });
        let task = classify_task(&body, 5, true, true, 0);
        assert_eq!(task, TaskType::ToolResult);
    }

    #[test]
    fn test_classify_debugging() {
        let body = json!({"messages": [{"role": "user", "content": "Fix this error"}]});
        let task = classify_task(&body, 5, true, false, 3);
        assert_eq!(task, TaskType::Debugging);
    }

    #[test]
    fn test_classify_code_gen() {
        let body = json!({"messages": [
            {"role": "user", "content": "Add a new endpoint"},
            {"role": "assistant", "content": [{"type": "tool_use", "id": "1", "name": "edit", "input": {}}]},
            {"role": "user", "content": "looks good, continue"},
        ]});
        let task = classify_task(&body, 3, true, false, 0);
        assert_eq!(task, TaskType::CodeGeneration);
    }

    #[test]
    fn test_min_tier_planning() {
        let signals = RoutingSignals {
            blast_radius_score: None,
            task_type: TaskType::Planning,
            recent_error_count: 0,
            message_count: 1,
            has_tool_history: false,
            is_tool_result_turn: false,
            estimated_tokens: 5000,
            requires_tool_support: false,
        };
        assert_eq!(determine_min_tier(&signals, 0.5), Tier::Premium);
    }

    #[test]
    fn test_min_tier_tool_result_free() {
        let signals = RoutingSignals {
            blast_radius_score: None,
            task_type: TaskType::ToolResult,
            recent_error_count: 0,
            message_count: 10,
            has_tool_history: true,
            is_tool_result_turn: true,
            estimated_tokens: 20000,
            requires_tool_support: false,
        };
        assert_eq!(determine_min_tier(&signals, 0.5), Tier::Free);
    }

    #[test]
    fn test_blast_radius_escalation() {
        let signals = RoutingSignals {
            blast_radius_score: Some(0.85),
            task_type: TaskType::ToolResult,
            recent_error_count: 0,
            message_count: 10,
            has_tool_history: true,
            is_tool_result_turn: true,
            estimated_tokens: 20000,
            requires_tool_support: false,
        };
        // Tool result is Free, but high blast radius escalates to Premium
        assert_eq!(determine_min_tier(&signals, 0.5), Tier::Premium);
    }

    #[test]
    fn test_blast_radius_moderate() {
        let signals = RoutingSignals {
            blast_radius_score: Some(0.55),
            task_type: TaskType::ToolResult,
            recent_error_count: 0,
            message_count: 10,
            has_tool_history: true,
            is_tool_result_turn: true,
            estimated_tokens: 20000,
            requires_tool_support: false,
        };
        // Moderate blast radius -> Mid tier
        assert_eq!(determine_min_tier(&signals, 0.5), Tier::Mid);
    }

    #[test]
    fn test_error_escalation_overrides() {
        let signals = RoutingSignals {
            blast_radius_score: None,
            task_type: TaskType::ToolResult,
            recent_error_count: 4,
            message_count: 10,
            has_tool_history: true,
            is_tool_result_turn: true,
            estimated_tokens: 20000,
            requires_tool_support: false,
        };
        // Error recovery overrides tool result's Free tier
        assert_eq!(determine_min_tier(&signals, 0.5), Tier::Premium);
    }

    #[test]
    fn test_large_context_escalation() {
        let signals = RoutingSignals {
            blast_radius_score: None,
            task_type: TaskType::ToolResult,
            recent_error_count: 0,
            message_count: 30,
            has_tool_history: true,
            is_tool_result_turn: true,
            estimated_tokens: 120_000,
            requires_tool_support: false,
        };
        // Large context needs Premium (Claude's 200K window)
        assert_eq!(determine_min_tier(&signals, 0.5), Tier::Premium);
    }

    #[test]
    fn test_legacy_decide_compatibility() {
        // Forced provider
        let decision = decide(&None, &Some("deepseek".into()));
        assert_eq!(decision.provider, "deepseek");
        assert_eq!(decision.reason, "forced");

        // No body
        let decision = decide(&None, &None);
        assert_eq!(decision.provider, "claude");

        // Planning (<=2 messages, no tools)
        let body = json!({"messages": [{"role": "user", "content": "Hello"}]});
        let decision = decide(&Some(body), &None);
        assert_eq!(decision.provider, "claude");
        assert_eq!(decision.tier, Tier::Premium);

        // Tool result -> DeepSeek
        let body = json!({"messages": [
            {"role": "user", "content": "hi"},
            {"role": "assistant", "content": [{"type": "tool_use", "id": "1", "name": "read", "input": {}}]},
            {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "1", "content": "data"}]},
        ]});
        let decision = decide(&Some(body), &None);
        assert_eq!(decision.provider, "deepseek");
    }

    #[tokio::test]
    async fn test_decide_with_registry() {
        let registry = crate::provider::build_registry("sk-test", "", &[]);

        // Planning -> Claude (Premium)
        let body = json!({"messages": [{"role": "user", "content": "Plan the auth system"}]});
        let decision = decide_with_registry(&Some(body), &None, None, 0.5, &registry).await;
        assert_eq!(decision.provider, "claude");
        assert_eq!(decision.tier, Tier::Premium);

        // Tool result -> DeepSeek (cheapest at Free/Mid)
        let body = json!({"messages": [
            {"role": "user", "content": "hi"},
            {"role": "assistant", "content": [{"type": "tool_use", "id": "1", "name": "read", "input": {}}]},
            {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "1", "content": "data"}]},
        ]});
        let decision = decide_with_registry(&Some(body), &None, None, 0.5, &registry).await;
        assert_eq!(decision.provider, "deepseek");

        // Tool result with high blast radius -> Claude
        let decision = decide_with_registry(&Some(json!({"messages": [
            {"role": "user", "content": "hi"},
            {"role": "assistant", "content": [{"type": "tool_use", "id": "1", "name": "edit", "input": {}}]},
            {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "1", "content": "ok"}]},
        ]})), &None, Some(0.85), 0.5, &registry).await;
        assert_eq!(decision.provider, "claude");
    }
}
