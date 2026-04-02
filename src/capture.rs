//! Conversation capture — extracts and stores full conversation turns.
//!
//! Every request/response pair through the proxy becomes a searchable turn.
//! Captures: user message, assistant response, tool invocations, metadata.

use crate::db::Db;
use crate::embeddings::{self, EmbeddingEngine};
use rusqlite::params;
use serde_json::Value;
use std::sync::Arc;
use tracing::{debug, info, warn};

/// Data for a single conversation turn.
pub struct TurnData {
    pub user_message: String,
    pub assistant_message: String,
    pub tools: Vec<(String, String)>, // (tool_name, tool_input_summary)
    pub provider: String,
    pub model: String,
    pub route_reason: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub latency_ms: u64,
    pub cost: f64,
}

/// Extract the user's message from the request body.
/// Gets only text blocks from the LAST user message (skips tool_results).
pub fn extract_user_message(body: &Value) -> Option<String> {
    let messages = body.get("messages")?.as_array()?;

    // Find the last user message
    for msg in messages.iter().rev() {
        if msg.get("role")?.as_str()? != "user" {
            continue;
        }

        // Simple string content
        if let Some(text) = msg.get("content").and_then(|c| c.as_str()) {
            if !text.is_empty() {
                return Some(text.to_string());
            }
        }

        // Array content — extract text blocks, skip tool_results
        if let Some(blocks) = msg.get("content").and_then(|c| c.as_array()) {
            let texts: Vec<&str> = blocks.iter()
                .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
                .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                .collect();

            if !texts.is_empty() {
                return Some(texts.join("\n"));
            }
        }
    }

    None
}

/// Assemble the assistant's response from SSE stream data.
/// Extracts text from content_block_delta events.
pub fn assemble_response(stream_data: &str) -> String {
    let mut response = String::new();

    for line in stream_data.lines() {
        if let Some(json_str) = line.strip_prefix("data: ") {
            if let Ok(event) = serde_json::from_str::<Value>(json_str) {
                // content_block_delta with text
                if event.get("type").and_then(|t| t.as_str()) == Some("content_block_delta") {
                    if let Some(text) = event.get("delta")
                        .and_then(|d| d.get("text"))
                        .and_then(|t| t.as_str())
                    {
                        response.push_str(text);
                    }
                }
            }
        }
    }

    response
}

/// Extract tool invocations from the request body.
/// Looks at assistant messages for tool_use blocks.
pub fn extract_tool_calls(body: &Value) -> Vec<(String, String)> {
    let mut tools = Vec::new();

    let Some(messages) = body.get("messages").and_then(|m| m.as_array()) else {
        return tools;
    };

    // Look at recent assistant messages for tool_use blocks
    for msg in messages.iter().rev().take(4) {
        if msg.get("role").and_then(|r| r.as_str()) != Some("assistant") {
            continue;
        }

        let Some(blocks) = msg.get("content").and_then(|c| c.as_array()) else {
            continue;
        };

        for block in blocks {
            if block.get("type").and_then(|t| t.as_str()) != Some("tool_use") {
                continue;
            }

            let tool_name = block.get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("unknown")
                .to_string();

            let input = block.get("input");
            let tool_input = match tool_name.as_str() {
                "Read" | "read" => input
                    .and_then(|i| i.get("file_path"))
                    .and_then(|p| p.as_str())
                    .unwrap_or("?")
                    .to_string(),
                "Edit" | "edit" | "Write" | "write" => input
                    .and_then(|i| i.get("file_path"))
                    .and_then(|p| p.as_str())
                    .unwrap_or("?")
                    .to_string(),
                "Bash" | "bash" => {
                    let cmd = input
                        .and_then(|i| i.get("command"))
                        .and_then(|c| c.as_str())
                        .unwrap_or("?");
                    if cmd.len() > 80 { format!("{}...", &cmd[..80]) } else { cmd.to_string() }
                }
                "Grep" | "grep" => input
                    .and_then(|i| i.get("pattern"))
                    .and_then(|p| p.as_str())
                    .unwrap_or("?")
                    .to_string(),
                "Glob" | "glob" => input
                    .and_then(|i| i.get("pattern"))
                    .and_then(|p| p.as_str())
                    .unwrap_or("?")
                    .to_string(),
                _ => format!("{}", tool_name),
            };

            tools.push((tool_name, tool_input));
        }
    }

    tools
}

/// Get or create a conversation for this project.
pub async fn get_or_create_conversation(db: &Db, project: &str, is_new_session: bool) -> i64 {
    let db = db.clone();
    let project = project.to_string();

    tokio::task::spawn_blocking(move || {
        let conn = db.blocking_lock();

        if !is_new_session {
            // Try to find the most recent active conversation for this project
            if let Ok(id) = conn.query_row(
                "SELECT id FROM conversations WHERE project_path = ?1 AND ended_at IS NULL ORDER BY started_at DESC LIMIT 1",
                params![project],
                |row| row.get::<_, i64>(0),
            ) {
                return id;
            }
        }

        // Create new conversation
        conn.execute(
            "INSERT INTO conversations (project_path) VALUES (?1)",
            params![project],
        ).ok();

        conn.last_insert_rowid()
    }).await.unwrap_or(0)
}

/// Store a completed turn with embedding.
pub async fn store_turn(
    db: &Db,
    embedder: Option<&EmbeddingEngine>,
    conversation_id: i64,
    turn: &TurnData,
) {
    // Generate embedding (blocking — CPU bound)
    let embed_text = format!("{} {}", turn.user_message, turn.assistant_message);
    let embedding_blob = if let Some(engine) = embedder {
        let text = embed_text.clone();
        match tokio::task::spawn_blocking(move || {
            // Can't move engine into blocking task, so we'll handle this differently
            Ok::<_, String>(Vec::<u8>::new()) // Placeholder — see note below
        }).await {
            Ok(Ok(blob)) => Some(blob),
            _ => None,
        }
    } else {
        None
    };

    // For now, embed synchronously (will optimize later with Arc<EmbeddingEngine>)
    let embedding_blob = embedder.and_then(|e| {
        e.embed(&embed_text).ok().map(|v| embeddings::vec_to_blob(&v))
    });

    let db = db.clone();
    let turn_user = turn.user_message.clone();
    let turn_asst = turn.assistant_message.clone();
    let turn_provider = turn.provider.clone();
    let turn_model = turn.model.clone();
    let turn_reason = turn.route_reason.clone();
    let tools = turn.tools.clone();
    let input_tokens = turn.input_tokens;
    let output_tokens = turn.output_tokens;
    let latency = turn.latency_ms;
    let cost = turn.cost;

    tokio::task::spawn_blocking(move || {
        let conn = db.blocking_lock();

        // Insert turn
        conn.execute(
            "INSERT INTO turns (conversation_id, turn_number, user_message, assistant_message,
             provider, model, route_reason, input_tokens, output_tokens, latency_ms, cost, embedding)
             VALUES (?1, (SELECT COALESCE(MAX(turn_number),0)+1 FROM turns WHERE conversation_id=?1),
             ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                conversation_id, turn_user, turn_asst,
                turn_provider, turn_model, turn_reason,
                input_tokens, output_tokens, latency, cost,
                embedding_blob,
            ],
        ).ok();

        let turn_id = conn.last_insert_rowid();

        // Insert tool invocations
        for (name, input) in &tools {
            conn.execute(
                "INSERT INTO tool_invocations (turn_id, tool_name, tool_input) VALUES (?1, ?2, ?3)",
                params![turn_id, name, input],
            ).ok();
        }

        // Update FTS index
        conn.execute(
            "INSERT INTO turns_fts (rowid, user_message, assistant_message) VALUES (?1, ?2, ?3)",
            params![turn_id, turn_user, turn_asst],
        ).ok();

        // Update conversation turn count
        conn.execute(
            "UPDATE conversations SET turn_count = turn_count + 1 WHERE id = ?1",
            params![conversation_id],
        ).ok();

        debug!(turn_id, conversation_id, "Turn stored");
    }).await.ok();
}
