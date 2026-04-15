//! Search across captured conversations.
//!
//! Combines semantic search (MiniLM embeddings) with keyword search (FTS5)
//! for the best of both worlds.

use crate::db::Db;
use crate::embeddings::EmbeddingEngine;
use rusqlite::params;
use tracing::debug;

#[derive(Debug, serde::Serialize)]
pub struct SearchResult {
    pub turn_id: i64,
    pub conversation_id: i64,
    pub timestamp: i64,
    pub project_path: String,
    pub user_message: String,
    pub assistant_snippet: String,
    pub tools_used: Vec<String>,
    pub similarity_score: f32,
    pub provider: String,
    pub cost: f64,
}

/// Combined semantic + keyword search.
pub async fn search(
    db: &Db,
    embedder: Option<&EmbeddingEngine>,
    query: &str,
    project: Option<&str>,
    limit: usize,
) -> Vec<SearchResult> {
    let mut results = Vec::new();

    // Semantic search (if embedder available)
    if let Some(engine) = embedder {
        let semantic = semantic_search(db, engine, query, project, limit).await;
        results.extend(semantic);
    }

    // Keyword search (always available)
    let keyword = keyword_search(db, query, project, limit).await;

    // Merge: add keyword results that aren't already in semantic results
    let existing_ids: std::collections::HashSet<i64> = results.iter().map(|r| r.turn_id).collect();
    for kr in keyword {
        if !existing_ids.contains(&kr.turn_id) {
            results.push(kr);
        }
    }

    // Sort by score descending
    results.sort_by(|a, b| b.similarity_score.partial_cmp(&a.similarity_score).unwrap_or(std::cmp::Ordering::Equal));
    results.truncate(limit);
    results
}

/// Semantic search using embeddings.
async fn semantic_search(
    db: &Db,
    embedder: &EmbeddingEngine,
    query: &str,
    project: Option<&str>,
    limit: usize,
) -> Vec<SearchResult> {
    // Embed the query
    let query_vec = match embedder.embed(query) {
        Ok(v) => v,
        Err(e) => {
            debug!(error = %e, "Embedding query failed");
            return Vec::new();
        }
    };

    // Load candidates from DB
    let db = db.clone();
    let project = project.map(|p| p.to_string());
    let top_k = limit;

    tokio::task::spawn_blocking(move || {
        let conn = db.blocking_lock();

        // Load turn IDs + embeddings
        // Limit candidates to most recent 500 turns to avoid loading unbounded embeddings into memory
        let candidates: Vec<(i64, Vec<u8>)> = {
            let sql = if project.is_some() {
                "SELECT t.id, t.embedding FROM turns t JOIN conversations c ON t.conversation_id = c.id WHERE t.embedding IS NOT NULL AND c.project_path = ?1 ORDER BY t.id DESC LIMIT 500"
            } else {
                "SELECT id, embedding FROM turns WHERE embedding IS NOT NULL ORDER BY id DESC LIMIT 500"
            };
            let mut stmt = conn.prepare(sql).ok()?;
            if let Some(ref proj) = project {
                stmt.query_map(params![proj], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)))
                    .ok()?.filter_map(|r| r.ok()).collect()
            } else {
                stmt.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)))
                    .ok()?.filter_map(|r| r.ok()).collect()
            }
        };

        // Find top K by similarity
        let top = EmbeddingEngine::search_similar(&query_vec, &candidates, top_k);

        // Fetch full details for top results
        let mut results = Vec::new();
        for (turn_id, score) in top {
            if let Ok(result) = get_turn_detail(&conn, turn_id, score) {
                results.push(result);
            }
        }

        Some(results)
    }).await.ok().flatten().unwrap_or_default()
}

/// Keyword search using FTS5.
async fn keyword_search(
    db: &Db,
    query: &str,
    project: Option<&str>,
    limit: usize,
) -> Vec<SearchResult> {
    let db = db.clone();
    let query = query.to_string();
    let project = project.map(|p| p.to_string());

    tokio::task::spawn_blocking(move || {
        let conn = db.blocking_lock();

        let sql = if project.is_some() {
            "SELECT t.id, rank FROM turns_fts f
             JOIN turns t ON f.rowid = t.id
             JOIN conversations c ON t.conversation_id = c.id
             WHERE turns_fts MATCH ?1 AND c.project_path = ?2
             ORDER BY rank LIMIT ?3"
        } else {
            "SELECT t.id, rank FROM turns_fts f
             JOIN turns t ON f.rowid = t.id
             WHERE turns_fts MATCH ?1
             ORDER BY rank LIMIT ?2"
        };

        let mut stmt = conn.prepare(sql).ok()?;

        let rows: Vec<(i64, f64)> = if let Some(ref proj) = project {
            stmt.query_map(params![query, proj, limit as i64], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, f64>(1)?))
            }).ok()?.filter_map(|r| r.ok()).collect()
        } else {
            stmt.query_map(params![query, limit as i64], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, f64>(1)?))
            }).ok()?.filter_map(|r| r.ok()).collect()
        };

        let mut results = Vec::new();
        for (turn_id, rank) in rows {
            // Convert FTS5 rank to a 0-1 score (rank is negative, closer to 0 = better)
            let score = (1.0 / (1.0 + rank.abs())) as f32;
            if let Ok(result) = get_turn_detail(&conn, turn_id, score) {
                results.push(result);
            }
        }

        Some(results)
    }).await.ok().flatten().unwrap_or_default()
}

/// Get full turn details for a search result.
fn get_turn_detail(conn: &rusqlite::Connection, turn_id: i64, score: f32) -> Result<SearchResult, rusqlite::Error> {
    let (conv_id, timestamp, user_msg, asst_msg, provider, cost, project): (i64, i64, String, String, String, f64, String) =
        conn.query_row(
            "SELECT t.conversation_id, t.timestamp, COALESCE(t.user_message,''), COALESCE(t.assistant_message,''),
                    COALESCE(t.provider,''), COALESCE(t.cost,0), COALESCE(c.project_path,'')
             FROM turns t JOIN conversations c ON t.conversation_id = c.id
             WHERE t.id = ?1",
            params![turn_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?)),
        )?;

    // Get tool names
    let mut stmt = conn.prepare("SELECT tool_name FROM tool_invocations WHERE turn_id = ?1")?;
    let tools: Vec<String> = stmt.query_map(params![turn_id], |row| row.get(0))?
        .filter_map(|r| r.ok())
        .collect();

    // Truncate assistant message for snippet
    let snippet = if asst_msg.len() > 300 {
        let mut end = 300;
        while end > 0 && !asst_msg.is_char_boundary(end) { end -= 1; }
        format!("{}...", &asst_msg[..end])
    } else {
        asst_msg
    };

    Ok(SearchResult {
        turn_id,
        conversation_id: conv_id,
        timestamp,
        project_path: project,
        user_message: user_msg,
        assistant_snippet: snippet,
        tools_used: tools,
        similarity_score: score,
        provider,
        cost,
    })
}

/// Search for symbols by name using BM25 (FTS5) over the code graph.
/// Returns (symbol_name, kind, file_path, signature, blast_radius_score).
pub async fn search_symbols(
    db: &Db,
    query: &str,
    project: Option<&str>,
    limit: usize,
) -> Vec<crate::symbols::SymbolSearchHit> {
    use crate::symbols;
    let project_str = project.unwrap_or("").to_string();
    if project_str.is_empty() {
        return Vec::new();
    }
    symbols::search_symbols(db, &project_str, query, limit).await
        .into_iter()
        .map(|(name, kind, file, sig, score)| symbols::SymbolSearchHit {
            symbol_name: name,
            symbol_kind: kind,
            file_path: file,
            signature: sig,
            blast_radius_score: score,
        })
        .collect()
}

/// Get all turns in a conversation.
pub async fn get_conversation(db: &Db, conversation_id: i64) -> Vec<serde_json::Value> {
    let db = db.clone();
    tokio::task::spawn_blocking(move || {
        let conn = db.blocking_lock();
        let mut stmt = conn.prepare(
            "SELECT id, turn_number, timestamp, user_message, assistant_message, provider, model, input_tokens, output_tokens, cost
             FROM turns WHERE conversation_id = ?1 ORDER BY turn_number"
        ).ok()?;

        let rows: Vec<serde_json::Value> = stmt.query_map(params![conversation_id], |row| {
            Ok(serde_json::json!({
                "id": row.get::<_, i64>(0)?,
                "turn": row.get::<_, i64>(1)?,
                "timestamp": row.get::<_, i64>(2)?,
                "user": row.get::<_, String>(3)?,
                "assistant": row.get::<_, String>(4)?,
                "provider": row.get::<_, String>(5)?,
                "model": row.get::<_, String>(6)?,
                "input_tokens": row.get::<_, i64>(7)?,
                "output_tokens": row.get::<_, i64>(8)?,
                "cost": row.get::<_, f64>(9)?,
            }))
        }).ok()?.filter_map(|r| r.ok()).collect();

        Some(rows)
    }).await.ok().flatten().unwrap_or_default()
}

