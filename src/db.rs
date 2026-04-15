use rusqlite::{Connection, params};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{info, error};

pub type Db = Arc<Mutex<Connection>>;

/// Open (or create) the SQLite database and run migrations.
pub fn open(path: &Path) -> Result<Db, rusqlite::Error> {
    let conn = Connection::open(path)?;

    // Performance settings
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA busy_timeout = 5000;"
    )?;

    migrate(&conn)?;

    info!(path = %path.display(), "Database ready");
    Ok(Arc::new(Mutex::new(conn)))
}

fn migrate(conn: &Connection) -> Result<(), rusqlite::Error> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );"
    )?;

    let version: i64 = conn
        .query_row(
            "SELECT COALESCE((SELECT value FROM meta WHERE key = 'schema_version'), '0')",
            [],
            |row| row.get::<_, String>(0),
        )
        .unwrap_or_else(|_| "0".to_string())
        .parse()
        .unwrap_or(0);

    if version < 1 {
        info!("Running migration v1: requests + sessions tables");
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS requests (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp INTEGER NOT NULL DEFAULT (strftime('%s','now')),
                provider TEXT NOT NULL,
                model TEXT NOT NULL,
                route_reason TEXT,
                input_tokens INTEGER DEFAULT 0,
                output_tokens INTEGER DEFAULT 0,
                cache_read_tokens INTEGER DEFAULT 0,
                cache_write_tokens INTEGER DEFAULT 0,
                response_bytes INTEGER DEFAULT 0,
                latency_ms INTEGER DEFAULT 0,
                actual_cost REAL DEFAULT 0.0,
                claude_equiv_cost REAL DEFAULT 0.0,
                compressed_original INTEGER DEFAULT 0,
                compressed_final INTEGER DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS sessions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                started_at INTEGER NOT NULL DEFAULT (strftime('%s','now')),
                ended_at INTEGER,
                total_requests INTEGER DEFAULT 0,
                total_cost REAL DEFAULT 0.0
            );

            CREATE TABLE IF NOT EXISTS memories (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                project_path TEXT NOT NULL,
                content TEXT NOT NULL,
                category TEXT DEFAULT 'fact',
                relevance_score REAL DEFAULT 1.0,
                created_at INTEGER NOT NULL DEFAULT (strftime('%s','now')),
                last_accessed INTEGER NOT NULL DEFAULT (strftime('%s','now')),
                access_count INTEGER DEFAULT 0,
                UNIQUE(project_path, content)
            );

            CREATE INDEX IF NOT EXISTS idx_requests_timestamp ON requests(timestamp);
            CREATE INDEX IF NOT EXISTS idx_memories_project ON memories(project_path);
            CREATE INDEX IF NOT EXISTS idx_memories_relevance ON memories(relevance_score DESC);

            INSERT OR REPLACE INTO meta (key, value) VALUES ('schema_version', '1');
        ")?;
    }

    if version < 2 {
        info!("Running migration v2: conversations + turns + tool_invocations + FTS5");
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS conversations (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT,
                project_path TEXT NOT NULL,
                started_at INTEGER NOT NULL DEFAULT (strftime('%s','now')),
                ended_at INTEGER,
                turn_count INTEGER DEFAULT 0,
                summary TEXT
            );

            CREATE TABLE IF NOT EXISTS turns (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                conversation_id INTEGER REFERENCES conversations(id),
                turn_number INTEGER,
                timestamp INTEGER NOT NULL DEFAULT (strftime('%s','now')),
                user_message TEXT,
                assistant_message TEXT,
                provider TEXT,
                model TEXT,
                route_reason TEXT,
                input_tokens INTEGER DEFAULT 0,
                output_tokens INTEGER DEFAULT 0,
                latency_ms INTEGER DEFAULT 0,
                cost REAL DEFAULT 0,
                embedding BLOB
            );

            CREATE TABLE IF NOT EXISTS tool_invocations (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                turn_id INTEGER REFERENCES turns(id),
                tool_name TEXT NOT NULL,
                tool_input TEXT,
                is_error INTEGER DEFAULT 0
            );

            CREATE VIRTUAL TABLE IF NOT EXISTS turns_fts USING fts5(
                user_message, assistant_message,
                content=turns, content_rowid=id
            );

            CREATE INDEX IF NOT EXISTS idx_conversations_project ON conversations(project_path);
            CREATE INDEX IF NOT EXISTS idx_turns_conversation ON turns(conversation_id);
            CREATE INDEX IF NOT EXISTS idx_turns_timestamp ON turns(timestamp);
            CREATE INDEX IF NOT EXISTS idx_tool_invocations_turn ON tool_invocations(turn_id);
            CREATE INDEX IF NOT EXISTS idx_tool_invocations_name ON tool_invocations(tool_name);

            INSERT OR REPLACE INTO meta (key, value) VALUES ('schema_version', '2');
        ")?;
    }

    if version < 3 {
        info!("Running migration v3: file_hashes + dependencies + blast_radius + structural_summaries");
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS file_hashes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                project_path TEXT NOT NULL,
                file_path TEXT NOT NULL,
                sha256 TEXT NOT NULL,
                file_size INTEGER NOT NULL,
                language TEXT DEFAULT '',
                last_scanned INTEGER NOT NULL DEFAULT (strftime('%s','now')),
                UNIQUE(project_path, file_path)
            );

            CREATE TABLE IF NOT EXISTS file_dependencies (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                project_path TEXT NOT NULL,
                source_file TEXT NOT NULL,
                target_file TEXT NOT NULL,
                UNIQUE(project_path, source_file, target_file)
            );

            CREATE TABLE IF NOT EXISTS blast_radius_cache (
                project_path TEXT NOT NULL,
                file_path TEXT NOT NULL,
                direct_importers INTEGER DEFAULT 0,
                transitive_importers INTEGER DEFAULT 0,
                score REAL DEFAULT 0.0,
                computed_at INTEGER NOT NULL DEFAULT (strftime('%s','now')),
                PRIMARY KEY(project_path, file_path)
            );

            CREATE TABLE IF NOT EXISTS structural_summaries (
                content_hash TEXT PRIMARY KEY,
                file_path TEXT DEFAULT '',
                summary TEXT NOT NULL,
                original_lines INTEGER,
                summary_lines INTEGER,
                created_at INTEGER NOT NULL DEFAULT (strftime('%s','now'))
            );

            CREATE INDEX IF NOT EXISTS idx_file_hashes_project ON file_hashes(project_path);
            CREATE INDEX IF NOT EXISTS idx_deps_target ON file_dependencies(project_path, target_file);

            INSERT OR REPLACE INTO meta (key, value) VALUES ('schema_version', '3');
        ")?;
    }

    if version < 4 {
        info!("Running migration v4: compression_savings column");
        conn.execute_batch(
            "ALTER TABLE requests ADD COLUMN compression_savings REAL DEFAULT 0.0;
             INSERT OR REPLACE INTO meta (key, value) VALUES ('schema_version', '4');
            "
        )?;
    }

    if version < 5 {
        info!("Running migration v5: structured memory (topic, type, importance, temporal validity)");
        conn.execute_batch(
            "ALTER TABLE memories ADD COLUMN topic TEXT DEFAULT '';
             ALTER TABLE memories ADD COLUMN memory_type TEXT DEFAULT 'observation';
             ALTER TABLE memories ADD COLUMN importance INTEGER DEFAULT 0;
             ALTER TABLE memories ADD COLUMN valid_from INTEGER DEFAULT 0;
             ALTER TABLE memories ADD COLUMN valid_until INTEGER DEFAULT 0;
             CREATE INDEX IF NOT EXISTS idx_memories_topic ON memories(project_path, topic);
             CREATE INDEX IF NOT EXISTS idx_memories_type ON memories(memory_type);
             CREATE INDEX IF NOT EXISTS idx_memories_importance ON memories(importance DESC);
             INSERT OR REPLACE INTO meta (key, value) VALUES ('schema_version', '5');
            "
        )?;
    }

    if version < 6 {
        info!("Running migration v6: client column on turns table");
        conn.execute_batch(
            "ALTER TABLE turns ADD COLUMN client TEXT DEFAULT 'claude-code';
             INSERT OR REPLACE INTO meta (key, value) VALUES ('schema_version', '6');
            "
        )?;
    }

    if version < 7 {
        info!("Running migration v7: code_symbols + symbol_refs + symbols_fts + symbol_blast_radius (AST-based code graph)");
        conn.execute_batch(
            "-- Named symbols extracted from source files via Tree-sitter AST
             CREATE TABLE IF NOT EXISTS code_symbols (
                 id           INTEGER PRIMARY KEY AUTOINCREMENT,
                 project_path TEXT NOT NULL,
                 file_path    TEXT NOT NULL,
                 symbol_name  TEXT NOT NULL,
                 symbol_kind  TEXT NOT NULL,
                 start_line   INTEGER DEFAULT 0,
                 end_line     INTEGER DEFAULT 0,
                 is_exported  INTEGER DEFAULT 0,
                 signature    TEXT DEFAULT '',
                 UNIQUE(project_path, file_path, symbol_name, symbol_kind)
             );

             -- Cross-file symbol references (call graph, type refs, imports)
             CREATE TABLE IF NOT EXISTS symbol_refs (
                 id           INTEGER PRIMARY KEY AUTOINCREMENT,
                 project_path TEXT NOT NULL,
                 from_file    TEXT NOT NULL,
                 from_symbol  TEXT DEFAULT '',
                 to_file      TEXT NOT NULL,
                 to_symbol    TEXT NOT NULL,
                 ref_kind     TEXT NOT NULL,
                 UNIQUE(project_path, from_file, from_symbol, to_file, to_symbol, ref_kind)
             );

             -- BM25 keyword search over symbol names
             CREATE VIRTUAL TABLE IF NOT EXISTS symbols_fts USING fts5(
                 symbol_name, file_path, signature,
                 content=code_symbols, content_rowid=id
             );

             -- Symbol-level blast radius (more precise than file-level)
             CREATE TABLE IF NOT EXISTS symbol_blast_radius (
                 project_path       TEXT NOT NULL,
                 file_path          TEXT NOT NULL,
                 symbol_name        TEXT NOT NULL,
                 direct_callers     INTEGER DEFAULT 0,
                 transitive_callers INTEGER DEFAULT 0,
                 score              REAL DEFAULT 0.0,
                 computed_at        INTEGER NOT NULL DEFAULT (strftime('%s','now')),
                 PRIMARY KEY(project_path, file_path, symbol_name)
             );

             CREATE INDEX IF NOT EXISTS idx_symbols_project  ON code_symbols(project_path);
             CREATE INDEX IF NOT EXISTS idx_symbols_file     ON code_symbols(project_path, file_path);
             CREATE INDEX IF NOT EXISTS idx_symbols_name     ON code_symbols(project_path, symbol_name);
             CREATE INDEX IF NOT EXISTS idx_symrefs_from     ON symbol_refs(project_path, from_file, from_symbol);
             CREATE INDEX IF NOT EXISTS idx_symrefs_to       ON symbol_refs(project_path, to_file, to_symbol);
             CREATE INDEX IF NOT EXISTS idx_sym_blast_file   ON symbol_blast_radius(project_path, file_path);

             INSERT OR REPLACE INTO meta (key, value) VALUES ('schema_version', '7');
            "
        )?;
    }

    Ok(())
}

/// Record a completed request.
pub async fn log_request(
    db: &Db,
    provider: &str,
    model: &str,
    route_reason: &str,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
    response_bytes: u64,
    latency_ms: u64,
    actual_cost: f64,
    claude_equiv_cost: f64,
    compressed_original: u64,
    compressed_final: u64,
    compression_savings: f64,
) {
    let db = db.clone();
    let provider = provider.to_string();
    let model = model.to_string();
    let reason = route_reason.to_string();

    // Run blocking SQLite in a spawn_blocking to not block the async runtime
    tokio::task::spawn_blocking(move || {
        let conn = db.blocking_lock();
        if let Err(e) = conn.execute(
            "INSERT INTO requests (provider, model, route_reason, input_tokens, output_tokens,
             cache_read_tokens, cache_write_tokens, response_bytes, latency_ms,
             actual_cost, claude_equiv_cost, compressed_original, compressed_final, compression_savings)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                provider, model, reason,
                input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
                response_bytes, latency_ms, actual_cost, claude_equiv_cost,
                compressed_original, compressed_final, compression_savings
            ],
        ) {
            error!(error = %e, "Failed to log request");
        }
    }).await.ok();
}

/// Get aggregated stats from the database, optionally filtered by date range.
/// `from_ts` and `to_ts` are Unix timestamps. 0 means no filter on that end.
pub async fn get_stats(db: &Db, from_ts: i64, to_ts: i64) -> serde_json::Value {
    let db = db.clone();
    tokio::task::spawn_blocking(move || {
        let conn = db.blocking_lock();

        // Build WHERE clause for date filtering
        let where_clause = date_where(from_ts, to_ts);
        let w = if where_clause.is_empty() { String::new() } else { format!(" WHERE {}", where_clause) };
        let and_w = if where_clause.is_empty() { String::new() } else { format!(" AND {}", where_clause) };

        let total: i64 = conn.query_row(&format!("SELECT COUNT(*) FROM requests{}", w), [], |r| r.get(0)).unwrap_or(0);
        let claude: i64 = conn.query_row(&format!("SELECT COUNT(*) FROM requests WHERE provider='claude'{}", and_w), [], |r| r.get(0)).unwrap_or(0);
        let deepseek: i64 = conn.query_row(&format!("SELECT COUNT(*) FROM requests WHERE provider='deepseek'{}", and_w), [], |r| r.get(0)).unwrap_or(0);

        let total_input: i64 = conn.query_row(&format!("SELECT COALESCE(SUM(input_tokens),0) FROM requests{}", w), [], |r| r.get(0)).unwrap_or(0);
        let total_output: i64 = conn.query_row(&format!("SELECT COALESCE(SUM(output_tokens),0) FROM requests{}", w), [], |r| r.get(0)).unwrap_or(0);

        let actual_cost: f64 = conn.query_row(&format!("SELECT COALESCE(SUM(actual_cost),0) FROM requests{}", w), [], |r| r.get(0)).unwrap_or(0.0);
        let equiv_cost: f64 = conn.query_row(&format!("SELECT COALESCE(SUM(claude_equiv_cost),0) FROM requests{}", w), [], |r| r.get(0)).unwrap_or(0.0);
        let savings = equiv_cost - actual_cost;
        let savings_pct = if equiv_cost > 0.0 { (savings / equiv_cost) * 100.0 } else { 0.0 };

        let avg_latency: f64 = conn.query_row(&format!("SELECT COALESCE(AVG(latency_ms),0) FROM requests{}", w), [], |r| r.get(0)).unwrap_or(0.0);

        let comp_orig: i64 = conn.query_row(&format!("SELECT COALESCE(SUM(compressed_original),0) FROM requests{}", w), [], |r| r.get(0)).unwrap_or(0);
        let comp_final: i64 = conn.query_row(&format!("SELECT COALESCE(SUM(compressed_final),0) FROM requests{}", w), [], |r| r.get(0)).unwrap_or(0);
        let comp_count: i64 = conn.query_row(&format!("SELECT COUNT(*) FROM requests WHERE compressed_original > 0{}", and_w), [], |r| r.get(0)).unwrap_or(0);
        let comp_savings_total: f64 = conn.query_row(&format!("SELECT COALESCE(SUM(compression_savings),0) FROM requests{}", w), [], |r| r.get(0)).unwrap_or(0.0);

        let claude_tool_cost: f64 = conn.query_row(
            &format!("SELECT COALESCE(SUM(actual_cost),0) FROM requests WHERE provider='claude' AND route_reason IN ('tool-result','mid-session'){}", and_w),
            [], |r| r.get(0)
        ).unwrap_or(0.0);
        let potential_routing_savings = claude_tool_cost * 0.9;

        // Per-provider breakdown
        let mut providers = serde_json::Map::new();
        if let Ok(mut stmt) = conn.prepare(&format!(
            "SELECT provider, COUNT(*), COALESCE(SUM(actual_cost),0) FROM requests{} GROUP BY provider", w
        )) {
            let _ = stmt.query_map([], |row| {
                let name: String = row.get(0)?;
                let count: i64 = row.get(1)?;
                let cost: f64 = row.get(2)?;
                Ok((name, count, cost))
            }).ok().map(|rows| {
                for row in rows.flatten() {
                    providers.insert(row.0, serde_json::json!({"turns": row.1, "cost": (row.2 * 10000.0).round() / 10000.0}));
                }
            });
        }

        // Per-client breakdown (from turns table)
        let mut clients = serde_json::Map::new();
        if let Ok(mut stmt) = conn.prepare("SELECT COALESCE(client,'claude-code'), COUNT(*) FROM turns GROUP BY 1") {
            let _ = stmt.query_map([], |row| {
                let name: String = row.get(0)?;
                let count: i64 = row.get(1)?;
                Ok((name, count))
            }).ok().map(|rows| {
                for row in rows.flatten() {
                    clients.insert(row.0, serde_json::json!(row.1));
                }
            });
        }

        serde_json::json!({
            "total_requests": total,
            "claude_turns": claude,
            "deepseek_turns": deepseek,
            "total_input_tokens": total_input,
            "total_output_tokens": total_output,
            "actual_cost": (actual_cost * 10000.0).round() / 10000.0,
            "claude_equiv_cost": (equiv_cost * 10000.0).round() / 10000.0,
            "savings": (savings * 10000.0).round() / 10000.0,
            "savings_percent": (savings_pct * 10.0).round() / 10.0,
            "potential_routing_savings": (potential_routing_savings * 10000.0).round() / 10000.0,
            "avg_latency_ms": avg_latency.round() as i64,
            "compressed_requests": comp_count,
            "compressed_original_bytes": comp_orig,
            "compressed_final_bytes": comp_final,
            "compression_savings_total": (comp_savings_total * 10000.0).round() / 10000.0,
            "providers": providers,
            "clients": clients,
        })
    }).await.unwrap_or(serde_json::json!({"error": "db query failed"}))
}

/// Build a SQL WHERE fragment for timestamp filtering.
fn date_where(from_ts: i64, to_ts: i64) -> String {
    match (from_ts > 0, to_ts > 0) {
        (true, true) => format!("timestamp >= {} AND timestamp <= {}", from_ts, to_ts),
        (true, false) => format!("timestamp >= {}", from_ts),
        (false, true) => format!("timestamp <= {}", to_ts),
        (false, false) => String::new(),
    }
}

/// Get all memories.
pub async fn get_memories(db: &Db) -> Vec<serde_json::Value> {
    let db = db.clone();
    tokio::task::spawn_blocking(move || {
        let conn = db.blocking_lock();
        let mut stmt = conn.prepare(
            "SELECT id, project_path, content, category, relevance_score, created_at, access_count
             FROM memories ORDER BY relevance_score DESC LIMIT 100"
        ).ok()?;

        let rows = stmt.query_map([], |row| {
            Ok(serde_json::json!({
                "id": row.get::<_, i64>(0)?,
                "project": row.get::<_, String>(1)?,
                "content": row.get::<_, String>(2)?,
                "category": row.get::<_, String>(3)?,
                "relevance": row.get::<_, f64>(4)?,
                "created_at": row.get::<_, i64>(5)?,
                "accesses": row.get::<_, i64>(6)?,
            }))
        }).ok()?;

        Some(rows.filter_map(|r| r.ok()).collect::<Vec<_>>())
    }).await.ok().flatten().unwrap_or_default()
}

/// Reset all stats — deletes all request history and sessions.
/// Keeps memories and conversation captures intact.
pub async fn reset_stats(db: &Db) {
    let db = db.clone();
    tokio::task::spawn_blocking(move || {
        let conn = db.blocking_lock();
        conn.execute("DELETE FROM requests", []).ok();
        conn.execute("DELETE FROM sessions", []).ok();
        tracing::info!("Stats reset — all request history cleared");
    }).await.ok();
}

/// Clear all memories.
pub async fn clear_memories(db: &Db) {
    let db = db.clone();
    tokio::task::spawn_blocking(move || {
        let conn = db.blocking_lock();
        conn.execute("DELETE FROM memories", []).ok();
    }).await.ok();
}

/// Get recent requests for the dashboard, optionally filtered by date range.
pub async fn recent_requests(db: &Db, limit: usize, from_ts: i64, to_ts: i64) -> Vec<serde_json::Value> {
    let db = db.clone();
    tokio::task::spawn_blocking(move || {
        let conn = db.blocking_lock();

        let where_clause = date_where(from_ts, to_ts);
        let sql = if where_clause.is_empty() {
            format!(
                "SELECT timestamp, provider, model, route_reason, input_tokens, output_tokens,
                        latency_ms, actual_cost, claude_equiv_cost
                 FROM requests ORDER BY id DESC LIMIT {}", limit)
        } else {
            format!(
                "SELECT timestamp, provider, model, route_reason, input_tokens, output_tokens,
                        latency_ms, actual_cost, claude_equiv_cost
                 FROM requests WHERE {} ORDER BY id DESC LIMIT {}", where_clause, limit)
        };

        let mut stmt = conn.prepare(&sql).ok()?;

        let rows = stmt.query_map([], |row| {
            Ok(serde_json::json!({
                "timestamp": row.get::<_, i64>(0)?,
                "provider": row.get::<_, String>(1)?,
                "model": row.get::<_, String>(2)?,
                "reason": row.get::<_, String>(3)?,
                "input_tokens": row.get::<_, i64>(4)?,
                "output_tokens": row.get::<_, i64>(5)?,
                "latency_ms": row.get::<_, i64>(6)?,
                "actual_cost": row.get::<_, f64>(7)?,
                "equiv_cost": row.get::<_, f64>(8)?,
            }))
        }).ok()?;

        Some(rows.filter_map(|r| r.ok()).collect::<Vec<_>>())
    }).await.ok().flatten().unwrap_or_default()
}
