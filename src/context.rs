//! Context Window Management — 4-tier graduated approach.
//!
//! Prevents DeepSeek context overflow and optimizes token usage.

use serde_json::Value;
use tracing::{info, warn};

use crate::tokens;

/// Context management result.
pub struct ContextAction {
    /// Should we override the routing decision?
    pub force_provider: Option<&'static str>,
    /// Reason for context action (for logging)
    pub action: &'static str,
    /// Truncation limit for tool results (default 12000)
    pub truncation_limit: usize,
}

/// Analyze context size and decide what actions to take.
pub fn analyze(body: &Value) -> ContextAction {
    let estimated = tokens::estimate_tokens(body);

    // Tier 1: < 40K — no action needed
    if estimated < 40_000 {
        return ContextAction {
            force_provider: None,
            action: "none",
            truncation_limit: 12_000,
        };
    }

    // Tier 2: 40K-60K — aggressive compression (shorter truncation)
    if estimated < 60_000 {
        info!(tokens = estimated, "Context tier 2: aggressive compression");
        return ContextAction {
            force_provider: None,
            action: "aggressive-compress",
            truncation_limit: 6_000,
        };
    }

    // Tier 3: 60K-120K — force Claude (DeepSeek can't handle this well)
    if estimated < 120_000 {
        info!(tokens = estimated, "Context tier 3: forcing Claude route");
        return ContextAction {
            force_provider: Some("claude"),
            action: "force-claude",
            truncation_limit: 4_000,
        };
    }

    // Tier 4: > 120K — force Claude + trim oldest messages
    warn!(tokens = estimated, "Context tier 4: trimming old messages");
    ContextAction {
        force_provider: Some("claude"),
        action: "trim-and-claude",
        truncation_limit: 3_000,
    }
}

/// Trim oldest messages to fit within budget. Preserves:
/// - System prompt
/// - First user message
/// - Last N message pairs
pub fn trim_messages(body: &mut Value, target_tokens: usize) {
    let current_tokens = tokens::estimate_tokens(body);
    if current_tokens <= target_tokens {
        return;
    }
    let tokens_to_shed = current_tokens - target_tokens;

    let Some(messages) = body.get_mut("messages").and_then(|m| m.as_array_mut()) else {
        return;
    };

    if messages.len() <= 8 {
        return;
    }
    let mut shed = 0usize;

    // Find messages to remove: skip first 2 (system context) and last 8 (recent context)
    let removable_end = messages.len().saturating_sub(8);
    let removable_start = 2.min(removable_end);

    if removable_start >= removable_end {
        return;
    }

    // Build summary of what we're removing
    let mut summary_parts: Vec<String> = Vec::new();
    let mut indices_to_remove: Vec<usize> = Vec::new();

    for i in removable_start..removable_end {
        if shed >= tokens_to_shed {
            break;
        }

        let msg_tokens = message_token_estimate(&messages[i]);
        shed += msg_tokens;
        indices_to_remove.push(i);

        // Extract key info for summary
        if let Some(role) = messages[i]["role"].as_str() {
            if role == "assistant" {
                if let Some(text) = extract_first_text(&messages[i]) {
                    let preview = if text.len() > 100 { &text[..100] } else { &text };
                    summary_parts.push(format!("- {}", preview.replace('\n', " ")));
                }
            }
        }
    }

    if indices_to_remove.is_empty() {
        return;
    }

    let removed_count = indices_to_remove.len();

    // Remove in reverse order to preserve indices
    for &i in indices_to_remove.iter().rev() {
        messages.remove(i);
    }

    // Insert a summary message where the removed messages were
    let summary = format!(
        "[Context compressed: {} messages removed to fit context window.\nKey points from removed messages:\n{}]",
        removed_count,
        if summary_parts.is_empty() { "- (routine tool operations)".to_string() } else { summary_parts.join("\n") }
    );

    messages.insert(removable_start, serde_json::json!({
        "role": "user",
        "content": summary
    }));

    info!(removed = removed_count, shed_tokens = shed, "Trimmed old messages");
}

fn message_token_estimate(msg: &Value) -> usize {
    serde_json::to_string(msg).map(|s| s.len() / 4).unwrap_or(0)
}

fn extract_first_text(msg: &Value) -> Option<String> {
    if let Some(text) = msg["content"].as_str() {
        return Some(text.to_string());
    }
    if let Some(arr) = msg["content"].as_array() {
        for block in arr {
            if block["type"].as_str() == Some("text") {
                if let Some(t) = block["text"].as_str() {
                    return Some(t.to_string());
                }
            }
        }
    }
    None
}
