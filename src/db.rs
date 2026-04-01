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
             actual_cost, claude_equiv_cost, compressed_original, compressed_final)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                provider, model, reason,
                input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
                response_bytes, latency_ms, actual_cost, claude_equiv_cost,
                compressed_original, compressed_final
            ],
        ) {
            error!(error = %e, "Failed to log request");
        }
    }).await.ok();
}

/// Get aggregated stats from the database.
pub async fn get_stats(db: &Db) -> serde_json::Value {
    let db = db.clone();
    tokio::task::spawn_blocking(move || {
        let conn = db.blocking_lock();

        let total: i64 = conn.query_row("SELECT COUNT(*) FROM requests", [], |r| r.get(0)).unwrap_or(0);
        let claude: i64 = conn.query_row("SELECT COUNT(*) FROM requests WHERE provider='claude'", [], |r| r.get(0)).unwrap_or(0);
        let deepseek: i64 = conn.query_row("SELECT COUNT(*) FROM requests WHERE provider='deepseek'", [], |r| r.get(0)).unwrap_or(0);

        let total_input: i64 = conn.query_row("SELECT COALESCE(SUM(input_tokens),0) FROM requests", [], |r| r.get(0)).unwrap_or(0);
        let total_output: i64 = conn.query_row("SELECT COALESCE(SUM(output_tokens),0) FROM requests", [], |r| r.get(0)).unwrap_or(0);

        let actual_cost: f64 = conn.query_row("SELECT COALESCE(SUM(actual_cost),0) FROM requests", [], |r| r.get(0)).unwrap_or(0.0);
        let equiv_cost: f64 = conn.query_row("SELECT COALESCE(SUM(claude_equiv_cost),0) FROM requests", [], |r| r.get(0)).unwrap_or(0.0);
        let savings = equiv_cost - actual_cost;
        let savings_pct = if equiv_cost > 0.0 { (savings / equiv_cost) * 100.0 } else { 0.0 };

        let avg_latency: f64 = conn.query_row("SELECT COALESCE(AVG(latency_ms),0) FROM requests", [], |r| r.get(0)).unwrap_or(0.0);

        let comp_orig: i64 = conn.query_row("SELECT COALESCE(SUM(compressed_original),0) FROM requests", [], |r| r.get(0)).unwrap_or(0);
        let comp_final: i64 = conn.query_row("SELECT COALESCE(SUM(compressed_final),0) FROM requests", [], |r| r.get(0)).unwrap_or(0);
        let comp_count: i64 = conn.query_row("SELECT COUNT(*) FROM requests WHERE compressed_original > 0", [], |r| r.get(0)).unwrap_or(0);

        // Calculate potential savings: what if tool-result turns used DeepSeek?
        // Tool-result turns sent to Claude cost X, same tokens on DeepSeek would cost X * (0.27/3.0) for input
        let claude_tool_cost: f64 = conn.query_row(
            "SELECT COALESCE(SUM(actual_cost),0) FROM requests WHERE provider='claude' AND route_reason IN ('tool-result','mid-session')",
            [], |r| r.get(0)
        ).unwrap_or(0.0);
        // DeepSeek is ~10x cheaper than Sonnet for these turns
        let potential_routing_savings = claude_tool_cost * 0.9;

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
        })
    }).await.unwrap_or(serde_json::json!({"error": "db query failed"}))
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

/// Clear all memories.
pub async fn clear_memories(db: &Db) {
    let db = db.clone();
    tokio::task::spawn_blocking(move || {
        let conn = db.blocking_lock();
        conn.execute("DELETE FROM memories", []).ok();
    }).await.ok();
}

/// Get recent requests for the dashboard.
pub async fn recent_requests(db: &Db, limit: usize) -> Vec<serde_json::Value> {
    let db = db.clone();
    tokio::task::spawn_blocking(move || {
        let conn = db.blocking_lock();
        let mut stmt = conn.prepare(
            "SELECT timestamp, provider, model, route_reason, input_tokens, output_tokens,
                    latency_ms, actual_cost, claude_equiv_cost
             FROM requests ORDER BY id DESC LIMIT ?1"
        ).ok()?;

        let rows = stmt.query_map(params![limit as i64], |row| {
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
