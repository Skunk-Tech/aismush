//! Context Window Management — graduated approach.
//!
//! Handles the mismatch between Claude (200K) and DeepSeek (64K effective).
//! Never blocks the user — always finds a way to make the request work.

use serde_json::Value;
use tracing::{debug, info, warn};

use crate::tokens;

/// What the context manager decided to do.
pub struct ContextAction {
    /// Override routing? None = let router decide.
    pub force_provider: Option<&'static str>,
    /// What we did.
    pub action: &'static str,
    /// Whether we modified the body (need re-serialization).
    pub body_modified: bool,
}

/// Analyze context and prepare it for the target provider.
/// Called BEFORE routing — may influence routing decision.
pub fn prepare(body: &mut Value, target_provider: &str) -> ContextAction {
    let estimated = tokens::estimate_tokens(body);

    // Under 55K — both providers handle this fine
    if estimated < 55_000 {
        return ContextAction { force_provider: None, action: "none", body_modified: false };
    }

    // 55K-64K — DeepSeek's edge. Compress aggressively to try to fit.
    if estimated < 64_000 && target_provider == "deepseek" {
        info!(tokens = estimated, "Compressing to fit DeepSeek window");
        let trimmed = truncate_old_tool_results(body, 4_000);
        return ContextAction {
            force_provider: None,
            action: "compress-for-deepseek",
            body_modified: trimmed,
        };
    }

    // 64K-120K — too big for DeepSeek, prefer Claude but don't force it
    if estimated < 120_000 {
        info!(tokens = estimated, "Context large, preferring Claude");
        let trimmed = truncate_old_tool_results(body, 6_000);
        return ContextAction {
            force_provider: Some("claude"),
            action: "prefer-claude",
            body_modified: trimmed,
        };
    }

    // 120K-180K — trim old messages to bring it down
    if estimated < 180_000 {
        warn!(tokens = estimated, "Trimming old messages");
        truncate_old_tool_results(body, 3_000);
        trim_old_messages(body, 100_000);
        return ContextAction {
            force_provider: Some("claude"),
            action: "trim-old-messages",
            body_modified: true,
        };
    }

    // 180K+ — aggressive trim
    warn!(tokens = estimated, "Emergency context trim");
    truncate_old_tool_results(body, 2_000);
    trim_old_messages(body, 80_000);
    ContextAction {
        force_provider: Some("claude"),
        action: "emergency-trim",
        body_modified: true,
    }
}

/// After routing, if the chosen provider can't handle the context,
/// this function tries to make it work or suggests a fallback.
pub fn ensure_fits(body: &mut Value, provider: &str) -> bool {
    let estimated = tokens::estimate_tokens(body);
    let limit = if provider == "deepseek" { 60_000 } else { 190_000 };

    if estimated <= limit {
        return false; // Fits fine, no changes
    }

    // Doesn't fit — trim until it does
    let target = limit - 5_000; // Leave headroom
    truncate_old_tool_results(body, 2_000);
    trim_old_messages(body, target);

    let after = tokens::estimate_tokens(body);
    info!(before = estimated, after = after, provider, "Trimmed to fit provider window");
    true
}

/// Truncate tool_result content blocks in older messages.
/// Keeps recent ones (last 4 messages) at full size.
fn truncate_old_tool_results(body: &mut Value, max_chars: usize) -> bool {
    let Some(messages) = body.get_mut("messages").and_then(|m| m.as_array_mut()) else {
        return false;
    };

    let msg_count = messages.len();
    if msg_count <= 6 {
        return false; // Too few messages, don't touch
    }

    let mut modified = false;
    // Only truncate messages that aren't the last 4
    let cutoff = msg_count.saturating_sub(4);

    for msg in messages[..cutoff].iter_mut() {
        if msg.get("role").and_then(|r| r.as_str()) != Some("user") {
            continue;
        }
        let Some(content) = msg.get_mut("content").and_then(|c| c.as_array_mut()) else {
            continue;
        };

        for block in content.iter_mut() {
            if block.get("type").and_then(|t| t.as_str()) != Some("tool_result") {
                continue;
            }

            if let Some(text) = block.get("content").and_then(|c| c.as_str()).map(|s| s.to_string()) {
                if text.len() > max_chars {
                    let cut = text[..max_chars].rfind('\n').unwrap_or(max_chars);
                    block["content"] = Value::String(format!(
                        "{}\n[...{} chars truncated for context management]",
                        &text[..cut],
                        text.len() - cut
                    ));
                    modified = true;
                }
            }
        }
    }

    if modified {
        debug!(max_chars, "Truncated old tool results");
    }
    modified
}

/// Drop oldest messages to bring total under target token count.
/// Always preserves: system, first 2 messages, last 6 messages.
fn trim_old_messages(body: &mut Value, target_tokens: usize) {
    let current = tokens::estimate_tokens(body);
    if current <= target_tokens {
        return;
    }

    let Some(messages) = body.get_mut("messages").and_then(|m| m.as_array_mut()) else {
        return;
    };

    if messages.len() <= 10 {
        return; // Not enough to trim
    }

    let keep_start = 2; // First 2 messages (user's initial request + first response)
    let keep_end = 6;   // Last 6 messages (recent context)
    let removable_end = messages.len().saturating_sub(keep_end);

    if keep_start >= removable_end {
        return;
    }

    // Remove messages from the middle until we're under budget.
    // CRITICAL: always remove in PAIRS (assistant + user) to preserve
    // tool_use/tool_result pairing. Removing one without the other causes
    // Claude API 400 "tool use concurrency" errors.
    let mut to_remove = Vec::new();
    let mut removed_tokens = 0usize;
    let tokens_to_shed = current - target_tokens;

    let mut i = keep_start;
    while i < removable_end {
        if removed_tokens >= tokens_to_shed {
            break;
        }

        // Check if this is an assistant message with tool_use
        let has_tool_use = messages[i].get("role").and_then(|r| r.as_str()) == Some("assistant")
            && messages[i].get("content").and_then(|c| c.as_array()).map_or(false, |blocks| {
                blocks.iter().any(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_use"))
            });

        if has_tool_use && i + 1 < removable_end {
            // Remove the pair together (assistant tool_use + user tool_result)
            let size_a = serde_json::to_string(&messages[i]).map(|s| s.len() / 4).unwrap_or(0);
            let size_b = serde_json::to_string(&messages[i + 1]).map(|s| s.len() / 4).unwrap_or(0);
            removed_tokens += size_a + size_b;
            to_remove.push(i);
            to_remove.push(i + 1);
            i += 2;
        } else if has_tool_use {
            // Tool use at boundary without its result — skip it (don't orphan it)
            i += 1;
        } else {
            // Regular message (no tool_use) — safe to remove individually
            let msg_size = serde_json::to_string(&messages[i]).map(|s| s.len() / 4).unwrap_or(0);
            removed_tokens += msg_size;
            to_remove.push(i);
            i += 1;
        }
    }

    if to_remove.is_empty() {
        return;
    }

    let count = to_remove.len();

    // Remove in reverse order
    for &i in to_remove.iter().rev() {
        messages.remove(i);
    }

    // Insert summary where removed messages were
    messages.insert(keep_start, serde_json::json!({
        "role": "user",
        "content": format!("[Context management: {} earlier messages condensed to save {} tokens]", count, removed_tokens)
    }));

    info!(removed = count, tokens_freed = removed_tokens, "Trimmed old messages");
}
