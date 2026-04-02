#![allow(dead_code)]
//! Lightweight dependency graph and blast-radius analysis.
//!
//! Parses import statements via regex (no AST required) to build a project
//! dependency graph. Computes blast radius: how many files are affected when
//! a given file changes. Used as a routing signal — high blast radius files
//! get routed to Claude for careful handling.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use rusqlite::params;
use tracing::info;

use crate::db::Db;
use crate::hashes::FileHash;

pub struct DependencyGraph {
    /// file_path → list of files it imports
    pub imports: HashMap<String, Vec<String>>,
    /// file_path → list of files that import it (reverse index)
    pub importers: HashMap<String, Vec<String>>,
}

pub struct BlastRadius {
    pub direct_importers: usize,
    pub transitive_importers: usize,
    pub is_leaf: bool,
    pub is_shared: bool,
    pub score: f64,
}

/// Parse import statements from file content based on language.
/// Returns relative file paths (best-effort resolution).
pub fn parse_imports(content: &str, language: &str, file_path: &str) -> Vec<String> {
    match language {
        "rust" => parse_rust_imports(content, file_path),
        "typescript" | "javascript" => parse_ts_js_imports(content, file_path),
        "python" => parse_python_imports(content, file_path),
        "go" => parse_go_imports(content, file_path),
        _ => Vec::new(),
    }
}

/// Build the full dependency graph from a set of files.
pub fn build_graph(project_path: &Path, files: &[FileHash]) -> DependencyGraph {
    let mut imports: HashMap<String, Vec<String>> = HashMap::new();
    let mut importers: HashMap<String, Vec<String>> = HashMap::new();

    // Build set of known files for resolution
    let known_files: HashSet<&str> = files.iter().map(|f| f.file_path.as_str()).collect();

    for file in files {
        let full_path = project_path.join(&file.file_path);
        let content = match std::fs::read_to_string(&full_path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let file_imports = parse_imports(&content, &file.language, &file.file_path);

        // Only keep imports that resolve to known project files
        let resolved: Vec<String> = file_imports.into_iter()
            .filter(|imp| known_files.contains(imp.as_str()))
            .collect();

        for target in &resolved {
            importers.entry(target.clone()).or_default().push(file.file_path.clone());
        }

        imports.insert(file.file_path.clone(), resolved);
    }

    DependencyGraph { imports, importers }
}

/// Compute blast radius for a single file.
pub fn blast_radius(graph: &DependencyGraph, file_path: &str) -> BlastRadius {
    let direct = graph.importers.get(file_path)
        .map(|v| v.len())
        .unwrap_or(0);

    // BFS for transitive importers
    let transitive = {
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        visited.insert(file_path.to_string());
        queue.push_back(file_path.to_string());

        while let Some(current) = queue.pop_front() {
            if let Some(deps) = graph.importers.get(&current) {
                for dep in deps {
                    if visited.insert(dep.clone()) {
                        queue.push_back(dep.clone());
                    }
                }
            }
        }
        visited.len().saturating_sub(1) // exclude self
    };

    let is_leaf = direct == 0;
    let is_shared = direct >= 5;

    // Score: 0.0 (leaf) to 1.0 (very shared)
    // Logarithmic scale: score = min(1.0, ln(1 + transitive) / ln(50))
    let score = if transitive == 0 {
        0.0
    } else {
        let raw = (1.0 + transitive as f64).ln() / (50.0_f64).ln();
        raw.min(1.0)
    };

    BlastRadius {
        direct_importers: direct,
        transitive_importers: transitive,
        is_leaf,
        is_shared,
        score,
    }
}

/// Store dependency graph and precomputed blast radii in the database.
pub async fn store_graph(db: &Db, project_path: &str, graph: &DependencyGraph) {
    let db = db.clone();
    let project = project_path.to_string();

    // Flatten edges
    let edges: Vec<(String, String)> = graph.imports.iter()
        .flat_map(|(src, targets)| targets.iter().map(move |t| (src.clone(), t.clone())))
        .collect();

    // Compute blast radii for all files
    let all_files: HashSet<&String> = graph.imports.keys()
        .chain(graph.importers.keys())
        .collect();
    let radii: Vec<(String, usize, usize, f64)> = all_files.iter()
        .map(|f| {
            let br = blast_radius(graph, f);
            (f.to_string(), br.direct_importers, br.transitive_importers, br.score)
        })
        .collect();

    let high_impact = radii.iter().filter(|(_, _, _, s)| *s > 0.7).count();
    let total_edges = edges.len();
    let total_files = radii.len();

    tokio::task::spawn_blocking(move || {
        let conn = db.blocking_lock();

        // Clear old data for this project
        conn.execute("DELETE FROM file_dependencies WHERE project_path = ?1", params![project]).ok();
        conn.execute("DELETE FROM blast_radius_cache WHERE project_path = ?1", params![project]).ok();

        // Insert edges
        for (source, target) in &edges {
            conn.execute(
                "INSERT OR IGNORE INTO file_dependencies (project_path, source_file, target_file) VALUES (?1, ?2, ?3)",
                params![project, source, target],
            ).ok();
        }

        // Insert blast radii
        for (file_path, direct, transitive, score) in &radii {
            conn.execute(
                "INSERT OR REPLACE INTO blast_radius_cache (project_path, file_path, direct_importers, transitive_importers, score)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![project, file_path, *direct as i64, *transitive as i64, score],
            ).ok();
        }

        info!(files = total_files, edges = total_edges, high_impact = high_impact, "Dependency graph stored");
    }).await.ok();
}

/// Look up blast radius for a file from cache. Returns None if not cached.
pub async fn lookup_blast_radius(db: &Db, project_path: &str, file_path: &str) -> Option<BlastRadius> {
    let db = db.clone();
    let project = project_path.to_string();
    let file = file_path.to_string();

    tokio::task::spawn_blocking(move || {
        let conn = db.blocking_lock();
        conn.query_row(
            "SELECT direct_importers, transitive_importers, score FROM blast_radius_cache
             WHERE project_path = ?1 AND file_path = ?2",
            params![project, file],
            |row| {
                let direct: i64 = row.get(0)?;
                let transitive: i64 = row.get(1)?;
                let score: f64 = row.get(2)?;
                Ok(BlastRadius {
                    direct_importers: direct as usize,
                    transitive_importers: transitive as usize,
                    is_leaf: direct == 0,
                    is_shared: direct >= 5,
                    score,
                })
            },
        ).ok()
    }).await.ok().flatten()
}

// ── Rust import parsing ─────────────────────────────────────────────────

fn parse_rust_imports(content: &str, file_path: &str) -> Vec<String> {
    let mut results = Vec::new();
    let file_dir = Path::new(file_path).parent().unwrap_or(Path::new(""));

    for line in content.lines() {
        let trimmed = line.trim();

        // use crate::foo::bar → src/foo.rs or src/foo/mod.rs or src/foo/bar.rs
        if trimmed.starts_with("use crate::") {
            if let Some(rest) = trimmed.strip_prefix("use crate::") {
                let module = rest.split("::").next().unwrap_or("");
                let module = module.split('{').next().unwrap_or(module)
                    .trim_end_matches(';').trim();
                if !module.is_empty() {
                    results.push(format!("src/{}.rs", module));
                    results.push(format!("src/{}/mod.rs", module));
                }
            }
        }

        // mod foo; → sibling foo.rs or foo/mod.rs
        if (trimmed.starts_with("mod ") || trimmed.starts_with("pub mod ")) && trimmed.ends_with(';') {
            let name = trimmed
                .trim_start_matches("pub ")
                .trim_start_matches("mod ")
                .trim_end_matches(';')
                .trim();
            if !name.is_empty() {
                let dir = file_dir.to_string_lossy();
                if dir.is_empty() {
                    results.push(format!("{}.rs", name));
                    results.push(format!("{}/mod.rs", name));
                } else {
                    results.push(format!("{}/{}.rs", dir, name));
                    results.push(format!("{}/{}/mod.rs", dir, name));
                }
            }
        }
    }

    results
}

// ── TypeScript/JavaScript import parsing ────────────────────────────────

fn parse_ts_js_imports(content: &str, file_path: &str) -> Vec<String> {
    let mut results = Vec::new();
    let file_dir = Path::new(file_path).parent().unwrap_or(Path::new(""));

    for line in content.lines() {
        let trimmed = line.trim();

        // import ... from './foo' or '../foo'
        // require('./foo')
        let path = extract_js_import_path(trimmed);
        if let Some(rel) = path {
            if rel.starts_with('.') {
                let resolved = resolve_js_path(file_dir, &rel);
                results.extend(resolved);
            }
        }
    }

    results
}

fn extract_js_import_path(line: &str) -> Option<String> {
    // import ... from 'path'
    if line.contains(" from ") {
        let after_from = line.split(" from ").last()?;
        let path = after_from.trim().trim_matches(|c| c == '\'' || c == '"' || c == ';');
        return Some(path.to_string());
    }
    // require('path')
    if line.contains("require(") {
        let start = line.find("require(")? + 8;
        let rest = &line[start..];
        let end = rest.find(')')?;
        let path = rest[..end].trim().trim_matches(|c| c == '\'' || c == '"');
        return Some(path.to_string());
    }
    None
}

fn resolve_js_path(base_dir: &Path, rel_path: &str) -> Vec<String> {
    let resolved = base_dir.join(rel_path);
    let resolved_str = resolved.to_string_lossy().to_string();
    // Clean up ../ and ./
    let cleaned = normalize_path(&resolved_str);

    let extensions = ["", ".ts", ".tsx", ".js", ".jsx", "/index.ts", "/index.tsx", "/index.js"];
    extensions.iter()
        .map(|ext| format!("{}{}", cleaned, ext))
        .collect()
}

fn normalize_path(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').collect();
    let mut stack: Vec<&str> = Vec::new();
    for part in parts {
        match part {
            "." | "" => continue,
            ".." => { stack.pop(); }
            _ => stack.push(part),
        }
    }
    stack.join("/")
}

// ── Python import parsing ───────────────────────────────────────────────

fn parse_python_imports(content: &str, _file_path: &str) -> Vec<String> {
    let mut results = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();

        // from foo.bar import baz → foo/bar.py
        if trimmed.starts_with("from ") && trimmed.contains(" import") {
            let module = trimmed
                .trim_start_matches("from ")
                .split(" import").next()
                .unwrap_or("")
                .trim();
            if !module.starts_with('.') && !module.is_empty() {
                let path = module.replace('.', "/");
                results.push(format!("{}.py", path));
                results.push(format!("{}/__init__.py", path));
            }
        }

        // import foo.bar → foo/bar.py
        if trimmed.starts_with("import ") && !trimmed.contains(" as ") {
            let module = trimmed.trim_start_matches("import ").trim();
            let first = module.split(',').next().unwrap_or("").trim();
            if !first.is_empty() {
                let path = first.replace('.', "/");
                results.push(format!("{}.py", path));
                results.push(format!("{}/__init__.py", path));
            }
        }
    }

    results
}

// ── Go import parsing ───────────────────────────────────────────────────

fn parse_go_imports(content: &str, _file_path: &str) -> Vec<String> {
    let mut results = Vec::new();
    let mut in_import_block = false;

    // Try to find module path from go.mod context
    // For now, we just collect import paths — resolution happens at graph build time
    for line in content.lines() {
        let trimmed = line.trim();

        if trimmed == "import (" {
            in_import_block = true;
            continue;
        }
        if in_import_block {
            if trimmed == ")" {
                in_import_block = false;
                continue;
            }
            // Extract path from "path" or alias "path"
            if let Some(path) = extract_go_import_path(trimmed) {
                results.push(path);
            }
            continue;
        }

        if trimmed.starts_with("import ") {
            let rest = trimmed.trim_start_matches("import ").trim();
            if let Some(path) = extract_go_import_path(rest) {
                results.push(path);
            }
        }
    }

    results
}

fn extract_go_import_path(line: &str) -> Option<String> {
    let trimmed = line.trim();
    // Handle: alias "path" or just "path"
    let start = trimmed.find('"')? + 1;
    let end = trimmed[start..].find('"')? + start;
    let path = &trimmed[start..end];

    // Only keep project-relative imports (not stdlib)
    // Heuristic: contains at least one . (domain) or starts with module name
    // For simplicity, keep all — filter at graph build time against known files
    Some(path.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rust_imports() {
        let code = "use crate::config;\nuse crate::db::Db;\nmod utils;\npub mod helpers;";
        let imports = parse_rust_imports(code, "src/main.rs");
        assert!(imports.contains(&"src/config.rs".to_string()));
        assert!(imports.contains(&"src/db.rs".to_string()));
        assert!(imports.contains(&"src/utils.rs".to_string()));
        assert!(imports.contains(&"src/helpers.rs".to_string()));
    }

    #[test]
    fn test_ts_imports() {
        let code = "import { foo } from './utils';\nimport bar from '../lib/bar';";
        let imports = parse_ts_js_imports(code, "src/components/App.tsx");
        // Should resolve relative to src/components/
        assert!(imports.iter().any(|i| i.contains("utils")));
        assert!(imports.iter().any(|i| i.contains("bar")));
    }

    #[test]
    fn test_python_imports() {
        let code = "from utils.helpers import foo\nimport config";
        let imports = parse_python_imports(code, "main.py");
        assert!(imports.contains(&"utils/helpers.py".to_string()));
        assert!(imports.contains(&"config.py".to_string()));
    }

    #[test]
    fn test_blast_radius_leaf() {
        let mut graph = DependencyGraph {
            imports: HashMap::new(),
            importers: HashMap::new(),
        };
        graph.imports.insert("leaf.rs".to_string(), vec!["lib.rs".to_string()]);
        // leaf.rs imports lib.rs, but nothing imports leaf.rs
        let br = blast_radius(&graph, "leaf.rs");
        assert!(br.is_leaf);
        assert_eq!(br.direct_importers, 0);
        assert_eq!(br.score, 0.0);
    }

    #[test]
    fn test_blast_radius_shared() {
        let mut graph = DependencyGraph {
            imports: HashMap::new(),
            importers: HashMap::new(),
        };
        // types.rs is imported by 6 files
        graph.importers.insert("types.rs".to_string(), vec![
            "a.rs".to_string(), "b.rs".to_string(), "c.rs".to_string(),
            "d.rs".to_string(), "e.rs".to_string(), "f.rs".to_string(),
        ]);
        let br = blast_radius(&graph, "types.rs");
        assert!(!br.is_leaf);
        assert!(br.is_shared);
        assert_eq!(br.direct_importers, 6);
        assert!(br.score > 0.3);
    }
}
