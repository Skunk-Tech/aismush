#![allow(dead_code)]
//! File hashing and incremental change detection.
//!
//! SHA-256 hashes every code file in a project. On re-scan, computes a delta
//! of changed/new/deleted files so only changed files are re-analyzed.

use ring::digest;
use rusqlite::params;
use std::collections::HashMap;
use std::path::Path;
use tracing::debug;

use crate::db::Db;
use crate::scan::IGNORE_DIRS;

const MAX_FILE_SIZE: u64 = 1_048_576; // 1MB — skip larger files (likely binaries)

const BINARY_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "bmp", "ico", "svg", "webp",
    "woff", "woff2", "ttf", "eot", "otf",
    "mp3", "mp4", "wav", "ogg", "webm", "avi",
    "zip", "tar", "gz", "bz2", "xz", "7z", "rar",
    "wasm", "so", "dylib", "dll", "exe", "o", "a",
    "pdf", "doc", "docx", "xls", "xlsx", "ppt",
    "db", "sqlite", "sqlite3",
    "lock",
];

#[derive(Clone)]
pub struct FileHash {
    pub file_path: String,
    pub sha256: String,
    pub file_size: u64,
    pub language: String,
}

pub struct ScanDelta {
    pub changed: Vec<FileHash>,
    pub unchanged: Vec<String>,
    pub deleted: Vec<String>,
}

/// Compute SHA-256 of a file. Returns hex string + size.
pub fn hash_file(path: &Path) -> Option<(String, u64)> {
    let metadata = std::fs::metadata(path).ok()?;
    let size = metadata.len();
    if size > MAX_FILE_SIZE || size == 0 {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    let digest = digest::digest(&digest::SHA256, &bytes);
    let hex = digest.as_ref().iter().map(|b| format!("{:02x}", b)).collect::<String>();
    Some((hex, size))
}

/// Detect language from file extension.
pub fn detect_language(ext: &str) -> &'static str {
    match ext {
        "rs" => "rust",
        "py" => "python",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "ts" | "tsx" | "mts" => "typescript",
        "go" => "go",
        "java" => "java",
        "rb" => "ruby",
        "php" => "php",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" => "cpp",
        "cs" => "csharp",
        "swift" => "swift",
        "kt" | "kts" => "kotlin",
        "dart" => "dart",
        "ex" | "exs" => "elixir",
        "json" => "json",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        "md" | "markdown" => "markdown",
        "html" | "htm" => "html",
        "css" | "scss" | "sass" | "less" => "css",
        "sh" | "bash" | "zsh" => "shell",
        "sql" => "sql",
        _ => "unknown",
    }
}

/// Walk project, hash all code files, compare against stored hashes.
pub fn compute_delta(project_path: &Path, stored: &HashMap<String, String>) -> ScanDelta {
    let mut changed = Vec::new();
    let mut unchanged = Vec::new();
    let mut seen = std::collections::HashSet::new();

    walk_and_hash(project_path, project_path, &mut |rel_path, hash, size, ext| {
        let rel = rel_path.to_string();
        seen.insert(rel.clone());
        let language = detect_language(ext);

        if let Some(old_hash) = stored.get(&rel) {
            if *old_hash == hash {
                unchanged.push(rel);
            } else {
                changed.push(FileHash {
                    file_path: rel,
                    sha256: hash,
                    file_size: size,
                    language: language.to_string(),
                });
            }
        } else {
            // New file
            changed.push(FileHash {
                file_path: rel,
                sha256: hash,
                file_size: size,
                language: language.to_string(),
            });
        }
    });

    let deleted = stored.keys()
        .filter(|k| !seen.contains(k.as_str()))
        .cloned()
        .collect();

    ScanDelta { changed, unchanged, deleted }
}

fn walk_and_hash(
    root: &Path,
    dir: &Path,
    callback: &mut dyn FnMut(&str, String, u64, &str),
) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();

        if path.is_dir() {
            if IGNORE_DIRS.contains(&name.as_str()) || name.starts_with('.') {
                continue;
            }
            walk_and_hash(root, &path, callback);
        } else if path.is_file() {
            let ext = path.extension()
                .map(|e| e.to_string_lossy().to_string())
                .unwrap_or_default();

            if BINARY_EXTENSIONS.contains(&ext.as_str()) {
                continue;
            }

            if let Some(rel) = pathdiff(root, &path) {
                if let Some((hash, size)) = hash_file(&path) {
                    callback(&rel, hash, size, &ext);
                }
            }
        }
    }
}

fn pathdiff(root: &Path, path: &Path) -> Option<String> {
    path.strip_prefix(root).ok().map(|p| p.to_string_lossy().to_string())
}

/// Load stored hashes from the database for a project.
pub async fn load_stored_hashes(db: &Db, project_path: &str) -> HashMap<String, String> {
    let db = db.clone();
    let project = project_path.to_string();
    tokio::task::spawn_blocking(move || {
        let conn = db.blocking_lock();
        let mut stmt = conn.prepare(
            "SELECT file_path, sha256 FROM file_hashes WHERE project_path = ?1"
        ).ok()?;
        let rows = stmt.query_map(params![project], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        }).ok()?;
        Some(rows.filter_map(|r| r.ok()).collect::<HashMap<_, _>>())
    }).await.ok().flatten().unwrap_or_default()
}

/// Store computed hashes, replacing old entries for this project.
pub async fn store_hashes(db: &Db, project_path: &str, hashes: &[FileHash], deleted: &[String]) {
    let db = db.clone();
    let project = project_path.to_string();
    let hashes: Vec<(String, String, u64, String)> = hashes.iter()
        .map(|h| (h.file_path.clone(), h.sha256.clone(), h.file_size, h.language.clone()))
        .collect();
    let deleted: Vec<String> = deleted.to_vec();

    tokio::task::spawn_blocking(move || {
        let conn = db.blocking_lock();
        // Delete removed files
        for path in &deleted {
            conn.execute(
                "DELETE FROM file_hashes WHERE project_path = ?1 AND file_path = ?2",
                params![project, path],
            ).ok();
        }
        // Upsert changed/new files
        for (file_path, sha256, file_size, language) in &hashes {
            conn.execute(
                "INSERT OR REPLACE INTO file_hashes (project_path, file_path, sha256, file_size, language)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![project, file_path, sha256, *file_size as i64, language],
            ).ok();
        }
        debug!(project = %project, updated = hashes.len(), deleted = deleted.len(), "Hashes stored");
    }).await.ok();
}

/// Get files changed since a given timestamp (for memory injection).
pub async fn changed_since(db: &Db, project_path: &str, since_timestamp: i64) -> Vec<String> {
    let db = db.clone();
    let project = project_path.to_string();
    tokio::task::spawn_blocking(move || {
        let conn = db.blocking_lock();
        let mut stmt = conn.prepare(
            "SELECT file_path FROM file_hashes
             WHERE project_path = ?1 AND last_scanned > ?2
             ORDER BY last_scanned DESC LIMIT 20"
        ).ok()?;
        let rows = stmt.query_map(params![project, since_timestamp], |row| {
            row.get::<_, String>(0)
        }).ok()?;
        Some(rows.filter_map(|r| r.ok()).collect::<Vec<_>>())
    }).await.ok().flatten().unwrap_or_default()
}

/// Hash a string's content (for structural summary caching).
pub fn hash_content(content: &str) -> String {
    let digest = digest::digest(&digest::SHA256, content.as_bytes());
    digest.as_ref().iter().map(|b| format!("{:02x}", b)).collect()
}
