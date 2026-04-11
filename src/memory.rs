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
/// Extract observations from the request body (pure parsing, no I/O).
pub fn extract_observations(body: &Value) -> Vec<(String, String)> {
    let Some(messages) = body.get("messages").and_then(|m| m.as_array()) else {
        return Vec::new();
    };

    let recent = if messages.len() > 4 { &messages[messages.len()-4..] } else { messages.as_slice() };
    let mut observations: Vec<(String, String)> = Vec::new();

    for msg in recent {
        let Some(content) = msg.get("content").and_then(|c| c.as_array()) else { continue };

        for block in content {
            if block.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                let tool_name = block.get("name").and_then(|n| n.as_str()).unwrap_or("unknown");
                let input = block.get("input");

                let classifier = crate::tools::ToolClassifier::default();
                let category_enum = classifier.classify(tool_name, input);

                let observation = match &category_enum {
                    crate::tools::ToolCategory::FileRead => {
                        let path = input.and_then(|i| classifier.extract_file_path(i)).unwrap_or("?");
                        format!("Read file: {}", path)
                    }
                    crate::tools::ToolCategory::FileWrite => {
                        let path = input.and_then(|i| classifier.extract_file_path(i)).unwrap_or("?");
                        format!("Edited file: {}", path)
                    }
                    crate::tools::ToolCategory::Shell => {
                        let cmd = input.and_then(|i| classifier.extract_command(i)).unwrap_or("?");
                        let short = if cmd.len() > 100 { &cmd[..100] } else { cmd };
                        format!("Ran: {}", short)
                    }
                    crate::tools::ToolCategory::Search => {
                        let pattern = input.and_then(|i| classifier.extract_pattern(i)).unwrap_or("?");
                        format!("Searched for: {}", pattern)
                    }
                    crate::tools::ToolCategory::Other(_) => format!("Used tool: {}", tool_name),
                };

                let category = category_enum.observation_category();

                observations.push((observation, category.to_string()));
            }

            if msg.get("role").and_then(|r| r.as_str()) == Some("assistant") {
                if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                    if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                        for line in text.lines() {
                            let t = line.trim();
                            if t.len() > 30 && t.len() < 200 {
                                if t.contains("decided") || t.contains("chose") ||
                                   t.contains("switched to") || t.contains("will use") ||
                                   t.contains("going with") {
                                    observations.push((t.to_string(), "decision".to_string()));
                                } else if t.contains("fixed") || t.contains("resolved") ||
                                          t.contains("found the bug") || t.contains("root cause") ||
                                          t.contains("the issue was") || t.contains("realized") {
                                    observations.push((t.to_string(), "discovery".to_string()));
                                } else if t.contains("prefer") || t.contains("always use") ||
                                          t.contains("convention") || t.contains("don't like") {
                                    observations.push((t.to_string(), "preference".to_string()));
                                } else if t.contains("created") || t.contains("installed") ||
                                          t.contains("configured") || t.contains("changed") ||
                                          t.contains("updated") || t.contains("added") ||
                                          t.contains("removed") || t.contains("migrated") ||
                                          t.contains("refactored") {
                                    observations.push((t.to_string(), "decision".to_string()));
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    observations.sort_by(|a, b| a.0.cmp(&b.0));
    observations.dedup_by(|a, b| a.0 == b.0);
    observations.truncate(20);
    observations
}

/// Classify the topic of a memory based on its content.
pub fn classify_topic(content: &str) -> &'static str {
    let lower = content.to_lowercase();
    if lower.contains("auth") || lower.contains("login") || lower.contains("jwt") ||
       lower.contains("token") && lower.contains("session") || lower.contains("password") || lower.contains("oauth") {
        return "auth";
    }
    if lower.contains("database") || lower.contains("sql") || lower.contains("migration") ||
       lower.contains("schema") || lower.contains("query") || lower.contains("sqlite") || lower.contains("postgres") {
        return "database";
    }
    if lower.contains("frontend") || lower.contains("react") || lower.contains("vue") ||
       lower.contains("css") || lower.contains("html") || lower.contains("component") ||
       lower.contains("dashboard") && lower.contains("ui") {
        return "frontend";
    }
    if lower.contains("test") || lower.contains("assert") || lower.contains("cargo test") ||
       lower.contains("jest") || lower.contains("pytest") || lower.contains("spec") {
        return "testing";
    }
    if lower.contains("deploy") || lower.contains("release") || lower.contains("ci") ||
       lower.contains("docker") || lower.contains("kubernetes") || lower.contains("production") {
        return "deploy";
    }
    if lower.contains("config") || lower.contains("env") || lower.contains("setting") ||
       lower.contains("toml") || lower.contains(".yaml") || lower.contains("json config") {
        return "config";
    }
    if lower.contains("api") || lower.contains("endpoint") || lower.contains("route") ||
       lower.contains("handler") || lower.contains("request") && lower.contains("response") {
        return "api";
    }
    if lower.contains("build") || lower.contains("compile") || lower.contains("cargo") ||
       lower.contains("npm") || lower.contains("webpack") {
        return "build";
    }
    ""
}

/// Classify the type of a memory.
fn classify_type(content: &str, category: &str) -> &'static str {
    let lower = content.to_lowercase();
    if category == "decision" || lower.contains("decided") || lower.contains("chose") ||
       lower.contains("switched to") || lower.contains("will use") || lower.contains("going with") {
        return "decision";
    }
    if category == "discovery" || lower.contains("fixed") || lower.contains("resolved") ||
       lower.contains("found the bug") || lower.contains("root cause") {
        return "discovery";
    }
    if category == "preference" || lower.contains("prefer") || lower.contains("always use") ||
       lower.contains("convention") || lower.contains("don't like") {
        return "preference";
    }
    if lower.contains("released") || lower.contains("deployed") || lower.contains("completed") ||
       lower.contains("milestone") || lower.contains("launched") {
        return "event";
    }
    if category == "exploration" || category == "command" {
        return "observation";
    }
    "fact"
}

/// Score the importance of a memory (0=ephemeral, 1=normal, 2=important, 3=critical).
fn classify_importance(memory_type: &str, content: &str) -> i32 {
    match memory_type {
        "decision" | "discovery" | "preference" => 2,
        "event" => 1,
        "fact" => if content.len() > 50 { 1 } else { 0 },
        _ => 0, // observation
    }
}

/// Store pre-extracted observations in the database (background task).
pub async fn store_observations(db: &Db, project_path: &str, observations: Vec<(String, String)>) {
    if observations.is_empty() {
        return;
    }

    let count = observations.len();
    let db = db.clone();
    let project = project_path.to_string();

    tokio::task::spawn_blocking(move || {
        let conn = db.blocking_lock();
        for (content, category) in &observations {
            let topic = classify_topic(content);
            let memory_type = classify_type(content, category);
            let importance = classify_importance(memory_type, content);

            match conn.execute(
                "INSERT OR IGNORE INTO memories (project_path, content, category, topic, memory_type, importance, valid_from)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, strftime('%s','now'))",
                params![project, content, category, topic, memory_type, importance],
            ) {
                Ok(1) => debug!(content = &content[..content.len().min(60)], topic, memory_type, importance, "Memory stored"),
                Ok(_) => {}
                Err(e) => warn!(error = %e, "Failed to store memory"),
            }
        }
    }).await.ok();

    if count > 0 {
        debug!(count, project = project_path, "Captured observations");
    }
}

/// Inject relevant context from past conversations into the system prompt.
/// Uses a tiered approach with a strict token budget:
///   L0+L1: Critical facts (decisions, preferences, discoveries) — always loaded
///   L2: Topic-relevant memories — loaded if the current topic matches
///   L3: Recent conversation turns — loaded if budget remains
///
/// Total budget: ~300 tokens (1200 chars). This is small enough for any provider.
pub async fn inject_memories(db: &Db, body: &mut Value, project_path: &str) {
    const MAX_CHARS: usize = 1200; // ~300 tokens

    // Detect current topic from the last user message
    let current_topic = extract_user_topic(body);

    // L0+L1: Critical memories (importance >= 2: decisions, preferences, discoveries)
    let critical = fetch_critical_memories(db, project_path).await;

    // L2: Topic-relevant memories (if we detected a topic)
    let topic_mems = if !current_topic.is_empty() {
        fetch_topic_memories(db, project_path, &current_topic).await
    } else {
        Vec::new()
    };

    // L3: Recent turns
    let recent = get_recent_turns(db, project_path).await;

    if critical.is_empty() && topic_mems.is_empty() && recent.is_empty() {
        return;
    }

    let mut memory_text = String::from("[Project context:]\n");
    let mut count = 0;

    // L0+L1: Always inject critical facts
    for (content, memory_type) in &critical {
        let line = format!("- [{}] {}\n", memory_type, content);
        if memory_text.len() + line.len() > MAX_CHARS { break; }
        memory_text.push_str(&line);
        count += 1;
    }

    // L2: Topic-relevant (if budget remains)
    if !topic_mems.is_empty() && memory_text.len() < MAX_CHARS - 100 {
        memory_text.push_str(&format!("\n[{}:]\n", current_topic));
        for (content, _) in &topic_mems {
            let line = format!("- {}\n", content);
            if memory_text.len() + line.len() > MAX_CHARS { break; }
            memory_text.push_str(&line);
            count += 1;
        }
    }

    // L3: Recent turns (if budget remains)
    if !recent.is_empty() && memory_text.len() < MAX_CHARS - 200 {
        memory_text.push_str("\n[Recent:]\n");
        for (user_msg, asst_snippet, _tools, age) in &recent {
            let line = format!("  [{}] {}\n", age,
                if !user_msg.is_empty() { user_msg } else { asst_snippet });
            if memory_text.len() + line.len() > MAX_CHARS { break; }
            memory_text.push_str(&line);
            count += 1;
        }
    }

    if count == 0 {
        return;
    }

    // Inject into system prompt
    if let Some(system) = body.get_mut("system") {
        if let Some(text) = system.as_str().map(|s| s.to_string()) {
            *system = Value::String(format!("{}\n\n{}", memory_text, text));
        } else if let Some(arr) = system.as_array_mut() {
            // IMPORTANT: append AFTER existing blocks, not at position 0.
            //
            // Anthropic caches everything up to each cache_control marker. Inserting
            // memory at position 0 shifts all existing blocks — invalidating the cache
            // on every request and causing the full system prompt (~40K+ tokens) to be
            // counted as uncached input tokens (ITPM), triggering 429s.
            //
            // Appending at the end keeps the cached prefix intact. Only the ~300 token
            // memory block at the tail is uncached, costing negligible ITPM.
            arr.push(serde_json::json!({
                "type": "text",
                "text": memory_text,
            }));
        }
    }

    info!(count, chars = memory_text.len(), topic = %current_topic, project = project_path, "Injected tiered context");
}

/// Extract the current conversation topic from the last user message.
fn extract_user_topic(body: &Value) -> String {
    if let Some(msgs) = body.get("messages").and_then(|m| m.as_array()) {
        for msg in msgs.iter().rev() {
            if msg.get("role").and_then(|r| r.as_str()) != Some("user") { continue; }
            let text = match msg.get("content") {
                Some(Value::String(s)) => s.clone(),
                Some(Value::Array(blocks)) => blocks.iter()
                    .filter_map(|b| {
                        if b.get("type").and_then(|t| t.as_str()) == Some("text") {
                            b.get("text").and_then(|t| t.as_str()).map(|s| s.to_string())
                        } else { None }
                    })
                    .collect::<Vec<_>>().join(" "),
                _ => continue,
            };
            if !text.is_empty() {
                return classify_topic(&text).to_string();
            }
        }
    }
    String::new()
}

/// Fetch critical memories (importance >= 2, still valid).
async fn fetch_critical_memories(db: &Db, project_path: &str) -> Vec<(String, String)> {
    let db = db.clone();
    let project = project_path.to_string();
    tokio::task::spawn_blocking(move || {
        let conn = db.blocking_lock();
        let mut stmt = conn.prepare(
            "SELECT content, memory_type FROM memories
             WHERE project_path = ?1
               AND importance >= 2
               AND (valid_until = 0 OR valid_until > strftime('%s','now'))
             ORDER BY importance DESC, relevance_score DESC
             LIMIT 10"
        ).ok()?;
        let rows = stmt.query_map(params![project], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        }).ok()?;
        Some(rows.filter_map(|r| r.ok()).collect::<Vec<_>>())
    }).await.ok().flatten().unwrap_or_default()
}

/// Fetch memories matching a specific topic (importance >= 1, still valid).
async fn fetch_topic_memories(db: &Db, project_path: &str, topic: &str) -> Vec<(String, String)> {
    let db = db.clone();
    let project = project_path.to_string();
    let topic = topic.to_string();
    tokio::task::spawn_blocking(move || {
        let conn = db.blocking_lock();
        let mut stmt = conn.prepare(
            "SELECT content, memory_type FROM memories
             WHERE project_path = ?1
               AND topic = ?2
               AND importance >= 1
               AND (valid_until = 0 OR valid_until > strftime('%s','now'))
             ORDER BY relevance_score DESC, last_accessed DESC
             LIMIT 5"
        ).ok()?;
        let rows = stmt.query_map(params![project, topic], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        }).ok()?;
        Some(rows.filter_map(|r| r.ok()).collect::<Vec<_>>())
    }).await.ok().flatten().unwrap_or_default()
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
/// Decay relevance scores on startup.
pub async fn decay_scores(db: &Db) {
    let db = db.clone();
    tokio::task::spawn_blocking(move || {
        let conn = db.blocking_lock();
        conn.execute("UPDATE memories SET relevance_score = relevance_score * 0.95", []).ok();
        conn.execute("DELETE FROM memories WHERE relevance_score < 0.1 AND last_accessed < strftime('%s','now') - 2592000", []).ok();
    }).await.ok();
}

/// Periodic decay + prune for memories and turns (called every few hours).
pub async fn decay_and_prune(db: &Db) {
    let db = db.clone();
    tokio::task::spawn_blocking(move || {
        let conn = db.blocking_lock();
        // Decay memory scores
        conn.execute("UPDATE memories SET relevance_score = relevance_score * 0.95", []).ok();
        // Delete stale low-relevance memories (>30 days old, score < 0.1)
        conn.execute("DELETE FROM memories WHERE relevance_score < 0.1 AND last_accessed < strftime('%s','now') - 2592000", []).ok();
        // Expire ephemeral observations older than 7 days
        conn.execute("UPDATE memories SET valid_until = strftime('%s','now') WHERE memory_type = 'observation' AND valid_until = 0 AND created_at < strftime('%s','now') - 604800", []).ok();
        // Prune old turns without embeddings (>90 days)
        conn.execute("DELETE FROM turns WHERE embedding IS NULL AND timestamp < strftime('%s','now') - 7776000", []).ok();
        // Cap total turns to 10000 most recent
        conn.execute("DELETE FROM turns WHERE id NOT IN (SELECT id FROM turns ORDER BY id DESC LIMIT 10000)", []).ok();
        // Clean up orphaned tool_invocations
        conn.execute("DELETE FROM tool_invocations WHERE turn_id NOT IN (SELECT id FROM turns)", []).ok();
        tracing::info!("Database cleanup complete");
    }).await.ok();
}

/// Return the tiered memory context as plain text (for the SessionStart hook).
/// Same logic as inject_memories but returns a String instead of modifying a request body.
pub async fn get_context_text(db: &Db, project_path: &str) -> String {
    const MAX_CHARS: usize = 1200;

    let critical = fetch_critical_memories(db, project_path).await;
    let recent = get_recent_turns(db, project_path).await;

    if critical.is_empty() && recent.is_empty() {
        return String::new();
    }

    let mut text = format!("[Memory context — {}]\n", project_path);

    for (content, memory_type) in &critical {
        let line = format!("- [{}] {}\n", memory_type, content);
        if text.len() + line.len() > MAX_CHARS { break; }
        text.push_str(&line);
    }

    if !recent.is_empty() && text.len() < MAX_CHARS - 200 {
        text.push_str("\n[Recent work:]\n");
        for (user_msg, asst_snippet, _tools, age) in &recent {
            let msg = if !user_msg.is_empty() { user_msg.as_str() } else { asst_snippet.as_str() };
            let line = format!("  [{}] {}\n", age, msg);
            if text.len() + line.len() > MAX_CHARS { break; }
            text.push_str(&line);
        }
    }

    text
}

/// Summarize the most recent session for a project via AI and persist it.
/// Called from POST /memory/summarize (wired to the Stop hook).
pub async fn summarize_session(db: &Db, project_path: &str, proxy_port: u16, api_key: &str) -> Result<(), String> {
    let turns = get_turns_for_summary(db, project_path).await;
    if turns.is_empty() {
        return Ok(());
    }

    let mut context = String::new();
    for (user, asst, tools, age) in &turns {
        if !user.is_empty() {
            let mut end = user.len().min(200);
            while end > 0 && !user.is_char_boundary(end) { end -= 1; }
            context.push_str(&format!("[{}] User: {}\n", age, &user[..end]));
        }
        if !tools.is_empty() {
            context.push_str(&format!("  Tools: {}\n", tools));
        }
        if !asst.is_empty() {
            let mut end = asst.len().min(300);
            while end > 0 && !asst.is_char_boundary(end) { end -= 1; }
            context.push_str(&format!("  Assistant: {}\n\n", &asst[..end]));
        }
    }

    let prompt = format!(
        "Summarize this development session in 3-5 concise bullet points covering: \
         what was worked on, key decisions made, problems solved, and anything left incomplete.\n\n{}",
        context
    );

    // Use DeepSeek if key available (cheaper); fall back to Claude Haiku.
    let use_claude = api_key.is_empty();
    let summary = crate::scan::call_ai(&prompt, proxy_port, api_key, use_claude).await?;

    let db = db.clone();
    let project = project_path.to_string();
    tokio::task::spawn_blocking(move || {
        let conn = db.blocking_lock();
        conn.execute(
            "UPDATE conversations SET summary = ?1, ended_at = strftime('%s','now')
             WHERE id = (
                 SELECT id FROM conversations WHERE project_path = ?2
                 ORDER BY started_at DESC LIMIT 1
             )",
            params![summary, project],
        ).ok();
    }).await.ok();

    info!(project = project_path, "Session summary stored");
    Ok(())
}

/// Fetch the last 24 hours of turns for a project, ordered oldest-first (for summarization).
async fn get_turns_for_summary(db: &Db, project_path: &str) -> Vec<(String, String, String, String)> {
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
               AND t.timestamp > strftime('%s','now') - 86400
               AND t.user_message IS NOT NULL
               AND t.user_message != ''
             GROUP BY t.id
             ORDER BY t.timestamp ASC
             LIMIT 20"
        ).ok()?;

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64).unwrap_or(0);

        let rows = stmt.query_map(rusqlite::params![project], |row| {
            let user: String = row.get(0)?;
            let asst: String = row.get::<_, Option<String>>(1)?.unwrap_or_default();
            let ts: i64 = row.get(2)?;
            let tools: String = row.get::<_, Option<String>>(3)?.unwrap_or_default();
            let diff = now - ts;
            let age = if diff < 3600 { format!("{}m ago", diff / 60) }
                else if diff < 86400 { format!("{}h ago", diff / 3600) }
                else { format!("{}d ago", diff / 86400) };
            let snippet = if asst.len() > 300 {
                let mut end = 300;
                while end > 0 && !asst.is_char_boundary(end) { end -= 1; }
                format!("{}...", &asst[..end])
            } else { asst };
            Ok((user, snippet, tools, age))
        }).ok()?;

        Some(rows.filter_map(|r| r.ok()).collect::<Vec<_>>())
    }).await.ok().flatten().unwrap_or_default()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_topic_auth() {
        assert_eq!(classify_topic("Fix the JWT token validation"), "auth");
        assert_eq!(classify_topic("Add OAuth login flow"), "auth");
    }

    #[test]
    fn test_classify_topic_database() {
        assert_eq!(classify_topic("Run the SQL migration"), "database");
        assert_eq!(classify_topic("Fix SQLite query performance"), "database");
    }

    #[test]
    fn test_classify_topic_testing() {
        assert_eq!(classify_topic("cargo test is failing"), "testing");
        assert_eq!(classify_topic("Add unit tests for the parser"), "testing");
    }

    #[test]
    fn test_classify_topic_general() {
        assert_eq!(classify_topic("hello world"), "");
        assert_eq!(classify_topic("refactor the code"), "");
    }

    #[test]
    fn test_classify_type_decision() {
        assert_eq!(classify_type("decided to use React", ""), "decision");
        assert_eq!(classify_type("switched to async/await", ""), "decision");
        assert_eq!(classify_type("anything", "decision"), "decision");
    }

    #[test]
    fn test_classify_type_discovery() {
        assert_eq!(classify_type("fixed the null pointer bug", ""), "discovery");
        assert_eq!(classify_type("root cause was a race condition", ""), "discovery");
    }

    #[test]
    fn test_classify_type_preference() {
        assert_eq!(classify_type("prefer snake_case for Rust", ""), "preference");
        assert_eq!(classify_type("always use explicit types", ""), "preference");
    }

    #[test]
    fn test_classify_type_observation() {
        assert_eq!(classify_type("Read src/main.rs", "exploration"), "observation");
        assert_eq!(classify_type("ran cargo build", "command"), "observation");
    }

    #[test]
    fn test_classify_importance() {
        assert_eq!(classify_importance("decision", "anything"), 2);
        assert_eq!(classify_importance("discovery", "anything"), 2);
        assert_eq!(classify_importance("preference", "anything"), 2);
        assert_eq!(classify_importance("event", "anything"), 1);
        assert_eq!(classify_importance("observation", "anything"), 0);
        assert_eq!(classify_importance("fact", "short"), 0);
        assert_eq!(classify_importance("fact", "this is a longer fact that should have importance 1 because its over fifty chars"), 1);
    }
}
