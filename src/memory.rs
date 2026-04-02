//! Persistent cross-session memory (claude-mem inspired).
//!
//! Instead of pattern-matching response text (which doesn't work),
//! we capture tool_use/tool_result pairs from the request traffic.
//! Every file read, edit, command run, etc. is an observation.
//! On new sessions, recent observations are injected as context.

use crate::db::Db;
use rusqlite::params;
use serde_json::Value;
use tracing::{debug, info, warn};

/// Extract observations from the request body's messages.
/// Looks for tool_use blocks (what the AI did) and captures them.
pub async fn extract_from_request(db: &Db, project_path: &str, body: &Value) {
    let Some(messages) = body.get("messages").and_then(|m| m.as_array()) else {
        return;
    };

    // Only look at the last few messages for new observations
    // (older ones were already captured in previous requests)
    let recent = if messages.len() > 4 { &messages[messages.len()-4..] } else { messages.as_slice() };

    let mut observations: Vec<(String, String)> = Vec::new();

    for msg in recent {
        let Some(content) = msg.get("content").and_then(|c| c.as_array()) else { continue };

        for block in content {
            // Capture tool_use blocks (what the AI decided to do)
            if block.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                let tool_name = block.get("name").and_then(|n| n.as_str()).unwrap_or("unknown");
                let input = block.get("input");

                let observation = match tool_name {
                    "Read" | "read" => {
                        let path = input.and_then(|i| i.get("file_path")).and_then(|p| p.as_str()).unwrap_or("?");
                        format!("Read file: {}", path)
                    }
                    "Edit" | "edit" => {
                        let path = input.and_then(|i| i.get("file_path")).and_then(|p| p.as_str()).unwrap_or("?");
                        format!("Edited file: {}", path)
                    }
                    "Write" | "write" => {
                        let path = input.and_then(|i| i.get("file_path")).and_then(|p| p.as_str()).unwrap_or("?");
                        format!("Created file: {}", path)
                    }
                    "Bash" | "bash" => {
                        let cmd = input.and_then(|i| i.get("command")).and_then(|c| c.as_str()).unwrap_or("?");
                        // Truncate long commands
                        let short = if cmd.len() > 100 { &cmd[..100] } else { cmd };
                        format!("Ran: {}", short)
                    }
                    "Grep" | "grep" => {
                        let pattern = input.and_then(|i| i.get("pattern")).and_then(|p| p.as_str()).unwrap_or("?");
                        format!("Searched for: {}", pattern)
                    }
                    "Glob" | "glob" => {
                        let pattern = input.and_then(|i| i.get("pattern")).and_then(|p| p.as_str()).unwrap_or("?");
                        format!("Found files: {}", pattern)
                    }
                    _ => {
                        // Generic tool capture
                        format!("Used tool: {}", tool_name)
                    }
                };

                let category = match tool_name {
                    "Read" | "read" | "Grep" | "grep" | "Glob" | "glob" => "exploration",
                    "Edit" | "edit" | "Write" | "write" => "modification",
                    "Bash" | "bash" => "command",
                    _ => "tool",
                };

                observations.push((observation, category.to_string()));
            }

            // Capture key text from assistant messages (decisions, not fluff)
            if msg.get("role").and_then(|r| r.as_str()) == Some("assistant") {
                if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                    if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                        // Only capture short, decision-like statements
                        for line in text.lines() {
                            let t = line.trim();
                            if t.len() > 30 && t.len() < 200 {
                                // Look for decision/action indicators
                                if t.contains("created") || t.contains("installed") ||
                                   t.contains("configured") || t.contains("fixed") ||
                                   t.contains("changed") || t.contains("updated") ||
                                   t.contains("added") || t.contains("removed") ||
                                   t.contains("migrated") || t.contains("refactored") {
                                    observations.push((t.to_string(), "decision".to_string()));
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    if observations.is_empty() {
        return;
    }

    // Deduplicate
    observations.sort_by(|a, b| a.0.cmp(&b.0));
    observations.dedup_by(|a, b| a.0 == b.0);

    // Cap per request
    observations.truncate(20);

    let count = observations.len();
    let db = db.clone();
    let project = project_path.to_string();

    tokio::task::spawn_blocking(move || {
        let conn = db.blocking_lock();
        for (content, category) in &observations {
            match conn.execute(
                "INSERT OR IGNORE INTO memories (project_path, content, category) VALUES (?1, ?2, ?3)",
                params![project, content, category],
            ) {
                Ok(1) => debug!(content = &content[..content.len().min(60)], "Memory stored"),
                Ok(_) => {} // duplicate
                Err(e) => warn!(error = %e, "Failed to store memory"),
            }
        }
    }).await.ok();

    if count > 0 {
        debug!(count, project = project_path, "Captured observations");
    }
}

/// Inject relevant context from past conversations into the system prompt.
/// Uses deep memory (turns table) first, falls back to old memories table.
pub async fn inject_memories(db: &Db, body: &mut Value, project_path: &str) {
    // Try deep memory first (recent conversation turns)
    let deep_context = get_recent_turns(db, project_path).await;

    // Fall back to old memories table if no turns
    let old_memories = if deep_context.is_empty() {
        get_relevant_memories(db, project_path).await
    } else {
        Vec::new()
    };

    if deep_context.is_empty() && old_memories.is_empty() {
        return;
    }

    let mut memory_text = String::from("[Context from previous sessions:]\n");

    // Deep memory: recent conversation turns with actual reasoning
    if !deep_context.is_empty() {
        memory_text.push_str("\nRecent work:\n");
        for (user_msg, asst_snippet, tools, age) in &deep_context {
            memory_text.push_str(&format!("  [{}] You: {}\n", age, user_msg));
            if !asst_snippet.is_empty() {
                memory_text.push_str(&format!("    AI: {}\n", asst_snippet));
            }
            if !tools.is_empty() {
                memory_text.push_str(&format!("    Tools: {}\n", tools));
            }
        }
    }

    // Old memories as supplement
    if !old_memories.is_empty() {
        let mut by_category: std::collections::HashMap<&str, Vec<&str>> = std::collections::HashMap::new();
        for (content, category) in &old_memories {
            by_category.entry(category.as_str()).or_default().push(content.as_str());
        }
        for (category, items) in &by_category {
            memory_text.push_str(&format!("\n{}:\n", category));
            for item in items.iter().take(5) {
                memory_text.push_str(&format!("  - {}\n", item));
            }
        }
    }

    // Cap at ~3000 chars
    if memory_text.len() > 3000 {
        let mut end = 3000;
        while end > 0 && !memory_text.is_char_boundary(end) { end -= 1; }
        memory_text.truncate(end);
        memory_text.push_str("\n[...more context available via search]");
    }

    // Inject into system prompt
    if let Some(system) = body.get_mut("system") {
        if let Some(text) = system.as_str().map(|s| s.to_string()) {
            *system = Value::String(format!("{}\n\n{}", memory_text, text));
        } else if let Some(arr) = system.as_array_mut() {
            arr.insert(0, serde_json::json!({
                "type": "text",
                "text": memory_text,
            }));
        }
    }

    let count = deep_context.len() + old_memories.len();
    info!(count, project = project_path, "Injected context");
}

/// Get recent conversation turns for this project (last 7 days, max 10).
async fn get_recent_turns(db: &Db, project_path: &str) -> Vec<(String, String, String, String)> {
    let db = db.clone();
    let project = project_path.to_string();

    tokio::task::spawn_blocking(move || {
        let conn = db.blocking_lock();

        let mut stmt = conn.prepare(
            "SELECT t.user_message, t.assistant_message, t.timestamp,
                    GROUP_CONCAT(ti.tool_name, ', ') as tools
             FROM turns t
             JOIN conversations c ON t.conversation_id = c.id
             LEFT JOIN tool_invocations ti ON ti.turn_id = t.id
             WHERE c.project_path = ?1
               AND t.timestamp > strftime('%s','now') - 604800
               AND t.user_message IS NOT NULL
               AND t.user_message != ''
             GROUP BY t.id
             ORDER BY t.timestamp DESC
             LIMIT 10"
        ).ok()?;

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64).unwrap_or(0);

        let rows: Vec<(String, String, String, String)> = stmt.query_map(
            rusqlite::params![project],
            |row| {
                let user: String = row.get(0)?;
                let asst: String = row.get::<_, Option<String>>(1)?.unwrap_or_default();
                let ts: i64 = row.get(2)?;
                let tools: String = row.get::<_, Option<String>>(3)?.unwrap_or_default();

                // Format age
                let diff = now - ts;
                let age = if diff < 3600 { format!("{}m ago", diff / 60) }
                    else if diff < 86400 { format!("{}h ago", diff / 3600) }
                    else { format!("{}d ago", diff / 86400) };

                // Truncate assistant message
                let snippet = if asst.len() > 150 {
                    let mut end = 150;
                    while end > 0 && !asst.is_char_boundary(end) { end -= 1; }
                    format!("{}...", &asst[..end])
                } else {
                    asst
                };

                Ok((user, snippet, tools, age))
            }
        ).ok()?.filter_map(|r| r.ok()).collect();

        Some(rows)
    }).await.ok().flatten().unwrap_or_default()
}

/// Query top memories by relevance and recency.
async fn get_relevant_memories(db: &Db, project_path: &str) -> Vec<(String, String)> {
    let db = db.clone();
    let project = project_path.to_string();

    tokio::task::spawn_blocking(move || {
        let conn = db.blocking_lock();

        let mut stmt = conn.prepare(
            "SELECT content, category FROM memories
             WHERE project_path = ?1
             ORDER BY relevance_score * (1.0 / (1.0 + (strftime('%s','now') - last_accessed) / 604800.0)) DESC
             LIMIT 30"
        ).ok()?;

        let rows = stmt.query_map(params![project], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        }).ok()?;

        // Update access timestamps
        conn.execute(
            "UPDATE memories SET last_accessed = strftime('%s','now'), access_count = access_count + 1
             WHERE project_path = ?1",
            params![project],
        ).ok();

        Some(rows.filter_map(|r| r.ok()).collect::<Vec<_>>())
    }).await.ok().flatten().unwrap_or_default()
}

/// Decay relevance scores on startup.
pub async fn decay_scores(db: &Db) {
    let db = db.clone();
    tokio::task::spawn_blocking(move || {
        let conn = db.blocking_lock();
        conn.execute("UPDATE memories SET relevance_score = relevance_score * 0.95", []).ok();
        conn.execute("DELETE FROM memories WHERE relevance_score < 0.1 AND last_accessed < strftime('%s','now') - 2592000", []).ok();
    }).await.ok();
}

/// Detect project path from system prompt.
pub fn detect_project(body: &Value) -> String {
    if let Some(system) = body.get("system") {
        let text = if let Some(s) = system.as_str() {
            s.to_string()
        } else if let Some(arr) = system.as_array() {
            arr.iter()
                .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join(" ")
        } else {
            String::new()
        };

        for line in text.lines() {
            if line.contains("working directory:") || line.contains("Primary working directory:") {
                if let Some(path) = line.split(':').nth(1) {
                    let p = path.trim();
                    if !p.is_empty() {
                        return p.to_string();
                    }
                }
            }
        }
    }

    "default".to_string()
}
