//! Persistent cross-session memory.
//!
//! Extracts key facts from responses, stores in SQLite,
//! injects relevant memories into new sessions.

use crate::db::Db;
use rusqlite::params;
use serde_json::Value;
use tracing::{debug, info, warn};

/// Extract memorable facts from an assistant response and store them.
pub async fn extract_and_store(db: &Db, project_path: &str, response_text: &str) {
    let facts = extract_facts(response_text);
    if facts.is_empty() {
        return;
    }

    let db = db.clone();
    let project = project_path.to_string();
    let facts_clone = facts.clone();

    tokio::task::spawn_blocking(move || {
        let conn = db.blocking_lock();
        for (content, category) in &facts_clone {
            match conn.execute(
                "INSERT OR IGNORE INTO memories (project_path, content, category) VALUES (?1, ?2, ?3)",
                params![project, content, category],
            ) {
                Ok(1) => debug!(category, content = &content[..content.len().min(60)], "Memory stored"),
                Ok(_) => {} // duplicate, ignored
                Err(e) => warn!(error = %e, "Failed to store memory"),
            }
        }
    }).await.ok();

    info!(count = facts.len(), project = project_path, "Extracted memories");
}

/// Extract facts from response text using pattern matching.
fn extract_facts(text: &str) -> Vec<(String, String)> {
    let mut facts: Vec<(String, String)> = Vec::new();

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.len() < 20 || trimmed.len() > 500 {
            continue;
        }

        // Architecture decisions
        if let Some(fact) = match_pattern(trimmed, &[
            "I'll use ", "Let's use ", "We should use ", "We'll use ",
            "Using ", "Switching to ", "Migrating to ",
            "The architecture ", "The approach ",
        ]) {
            facts.push((fact, "architecture".into()));
            continue;
        }

        // Debugging facts
        if let Some(fact) = match_pattern(trimmed, &[
            "The issue was ", "The problem is ", "The bug was ",
            "Fixed by ", "The fix is ", "Root cause: ",
            "The error occurs because ", "This fails because ",
        ]) {
            facts.push((fact, "debugging".into()));
            continue;
        }

        // Project changes
        if let Some(fact) = match_pattern(trimmed, &[
            "Created file ", "Created ", "Added ",
            "Installed ", "Configured ", "Set up ",
            "Removed ", "Deleted ", "Renamed ",
        ]) {
            // Only if it looks like a file/project action (contains a path-like string)
            if fact.contains('/') || fact.contains('.') || fact.contains("module") || fact.contains("component") {
                facts.push((fact, "change".into()));
                continue;
            }
        }

        // Key decisions
        if let Some(fact) = match_pattern(trimmed, &[
            "Decision: ", "Decided to ", "The plan is to ",
            "Strategy: ", "We agreed to ",
        ]) {
            facts.push((fact, "decision".into()));
        }
    }

    // Deduplicate
    facts.sort_by(|a, b| a.0.cmp(&b.0));
    facts.dedup_by(|a, b| a.0 == b.0);

    // Cap at 10 per response to avoid noise
    facts.truncate(10);
    facts
}

fn match_pattern(text: &str, patterns: &[&str]) -> Option<String> {
    for pattern in patterns {
        if text.contains(pattern) {
            return Some(text.to_string());
        }
    }
    None
}

/// Get relevant memories for a project and inject into system prompt.
pub async fn inject_memories(db: &Db, body: &mut Value, project_path: &str) {
    let memories = get_relevant_memories(db, project_path).await;
    if memories.is_empty() {
        return;
    }

    // Build memory block
    let mut memory_text = String::from("[Project Memory — key facts from previous sessions:]\n");
    for (content, category) in &memories {
        memory_text.push_str(&format!("- [{}] {}\n", category, content));
    }

    // Cap at ~4000 chars (~1000 tokens)
    if memory_text.len() > 4000 {
        memory_text.truncate(4000);
        memory_text.push_str("\n[...more memories truncated]");
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

    info!(count = memories.len(), project = project_path, "Injected memories");
}

/// Query top memories by relevance × recency.
async fn get_relevant_memories(db: &Db, project_path: &str) -> Vec<(String, String)> {
    let db = db.clone();
    let project = project_path.to_string();

    tokio::task::spawn_blocking(move || {
        let conn = db.blocking_lock();

        // Relevance score × recency weight (decays over 7 days)
        let mut stmt = conn.prepare(
            "SELECT content, category FROM memories
             WHERE project_path = ?1
             ORDER BY relevance_score * (1.0 / (1.0 + (strftime('%s','now') - last_accessed) / 604800.0)) DESC
             LIMIT 20"
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

/// Decay memory relevance scores (call on startup).
pub async fn decay_scores(db: &Db) {
    let db = db.clone();
    tokio::task::spawn_blocking(move || {
        let conn = db.blocking_lock();
        conn.execute("UPDATE memories SET relevance_score = relevance_score * 0.95", []).ok();
        conn.execute("DELETE FROM memories WHERE relevance_score < 0.1 AND last_accessed < strftime('%s','now') - 2592000", []).ok();
    }).await.ok();
}

/// Detect project path from system prompt or working directory hints.
pub fn detect_project(body: &Value) -> String {
    // Look for working directory in system prompt
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

        // Look for common patterns
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
