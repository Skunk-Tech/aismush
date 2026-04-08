//! File content caching for repeated reads.
//!
//! Claude Code reads the same files repeatedly in a session. This module
//! caches file content hashes and replaces unchanged re-reads with a
//! compact marker (~10 tokens vs ~2000 tokens for a typical source file).

use std::collections::{HashMap, VecDeque};
use serde_json::Value;
use tracing::{debug, info};

/// Cache of file contents seen in tool_result blocks.
pub struct FileCache {
    entries: HashMap<String, CachedFile>,
    order: VecDeque<String>,
    max_entries: usize,
}

struct CachedFile {
    hash: u64,
    content_len: usize,
}

/// Result of checking a file against the cache.
pub enum CacheResult {
    /// File not in cache — first read.
    Miss,
    /// Content identical to cached version.
    Unchanged { original_len: usize },
    /// Content changed since last read.
    Changed,
}

/// Stats from applying file cache to a request body.
#[derive(Default, Debug)]
pub struct FileCacheStats {
    pub files_checked: usize,
    pub cache_hits: usize,
    pub bytes_saved: usize,
}

impl FileCache {
    pub fn new(max_entries: usize) -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            max_entries,
        }
    }

    /// Check if file content matches cache.
    pub fn check(&self, path: &str, content: &str) -> CacheResult {
        let new_hash = compute_hash(content);
        match self.entries.get(path) {
            None => CacheResult::Miss,
            Some(cached) => {
                if cached.hash == new_hash {
                    CacheResult::Unchanged { original_len: cached.content_len }
                } else {
                    CacheResult::Changed
                }
            }
        }
    }

    /// Insert or update a file in the cache.
    pub fn insert(&mut self, path: &str, content: &str) {
        let hash = compute_hash(content);
        let len = content.len();

        if self.entries.contains_key(path) {
            // Update existing entry
            self.entries.insert(path.to_string(), CachedFile { hash, content_len: len });
            // Move to back of LRU
            self.order.retain(|p| p != path);
            self.order.push_back(path.to_string());
        } else {
            // Evict oldest if at capacity
            if self.entries.len() >= self.max_entries {
                if let Some(old) = self.order.pop_front() {
                    self.entries.remove(&old);
                }
            }
            self.entries.insert(path.to_string(), CachedFile { hash, content_len: len });
            self.order.push_back(path.to_string());
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
    }
}

fn compute_hash(content: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    content.hash(&mut hasher);
    hasher.finish()
}

/// Apply file caching to a request body. Scans for Read tool_use/tool_result pairs
/// and replaces unchanged file re-reads with compact markers.
///
/// This runs BEFORE compression and applies to ALL providers (including Claude).
pub fn apply_file_cache(body: &mut Option<Value>, cache: &mut FileCache) -> FileCacheStats {
    let mut stats = FileCacheStats::default();

    let Some(body) = body.as_mut() else { return stats; };
    let Some(messages) = body.get_mut("messages").and_then(|m| m.as_array_mut()) else { return stats; };

    // IMPORTANT: Only process OLD messages (skip last 2).
    // The last assistant message contains tool_use blocks and the last user message
    // contains tool_result blocks. These are the ACTIVE turn — Claude needs them intact.
    // Modifying them causes API 400 "tool use concurrency" errors.
    let msg_count = messages.len();
    if msg_count < 3 { return stats; } // Not enough messages to have old reads
    let cutoff = msg_count - 2; // Skip last 2 messages (assistant tool_use + user tool_result)

    // Build a map of tool_use_id -> file_path from OLD assistant messages only
    let mut read_tool_map: HashMap<String, String> = HashMap::new();

    for msg in messages[..cutoff].iter() {
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
        if role != "assistant" { continue; }

        if let Some(blocks) = msg.get("content").and_then(|c| c.as_array()) {
            for block in blocks {
                if block.get("type").and_then(|t| t.as_str()) != Some("tool_use") { continue; }

                let name = block.get("name").and_then(|n| n.as_str()).unwrap_or("");
                // Match Read tool, read_file, cat — Claude Code's file reading tools
                if name != "Read" && name != "read_file" && name != "cat" && name != "read" {
                    continue;
                }

                let id = block.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string();
                let file_path = block.get("input")
                    .and_then(|i| i.get("file_path").or_else(|| i.get("path")))
                    .and_then(|p| p.as_str())
                    .unwrap_or("")
                    .to_string();

                if !id.is_empty() && !file_path.is_empty() {
                    read_tool_map.insert(id, file_path);
                }
            }
        }
    }

    if read_tool_map.is_empty() {
        return stats;
    }

    // Now scan OLD user messages for matching tool_results (same cutoff)
    for msg in messages[..cutoff].iter_mut() {
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
        if role != "user" { continue; }

        let Some(blocks) = msg.get_mut("content").and_then(|c| c.as_array_mut()) else { continue; };

        for block in blocks.iter_mut() {
            if block.get("type").and_then(|t| t.as_str()) != Some("tool_result") { continue; }

            let tool_use_id = block.get("tool_use_id").and_then(|i| i.as_str()).unwrap_or("").to_string();
            let Some(file_path) = read_tool_map.get(&tool_use_id) else { continue; };

            // Extract the text content
            let content = match block.get("content") {
                Some(Value::String(s)) => s.clone(),
                Some(Value::Array(arr)) => {
                    arr.iter()
                        .filter_map(|b| {
                            if b.get("type").and_then(|t| t.as_str()) == Some("text") {
                                b.get("text").and_then(|t| t.as_str()).map(|s| s.to_string())
                            } else { None }
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                }
                _ => continue,
            };

            if content.is_empty() || content.len() < 50 { continue; } // Don't cache tiny files

            stats.files_checked += 1;

            match cache.check(file_path, &content) {
                CacheResult::Miss => {
                    cache.insert(file_path, &content);
                    debug!(file = %file_path, bytes = content.len(), "File cached (first read)");
                }
                CacheResult::Unchanged { original_len } => {
                    let marker = format!("[File unchanged since last read — {} bytes cached]", original_len);
                    let saved = content.len().saturating_sub(marker.len());
                    stats.cache_hits += 1;
                    stats.bytes_saved += saved;

                    // Replace content with marker
                    match block.get_mut("content") {
                        Some(Value::String(s)) => {
                            *s = marker;
                        }
                        Some(Value::Array(arr)) => {
                            // Replace first text block, remove others
                            *arr = vec![serde_json::json!({"type": "text", "text": marker})];
                        }
                        _ => {}
                    }

                    debug!(file = %file_path, saved_bytes = saved, "File cache hit — replaced with marker");
                }
                CacheResult::Changed => {
                    cache.insert(file_path, &content);
                    debug!(file = %file_path, "File changed — cache updated");
                }
            }
        }
    }

    // Second pass: scan RECENT messages to populate cache (but don't modify content)
    // This ensures the next request can benefit from caching on these files
    let mut recent_tool_map: HashMap<String, String> = HashMap::new();
    for msg in messages[cutoff..].iter() {
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
        if role != "assistant" { continue; }
        if let Some(blocks) = msg.get("content").and_then(|c| c.as_array()) {
            for block in blocks {
                if block.get("type").and_then(|t| t.as_str()) != Some("tool_use") { continue; }
                let name = block.get("name").and_then(|n| n.as_str()).unwrap_or("");
                if name != "Read" && name != "read_file" && name != "cat" && name != "read" { continue; }
                let id = block.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string();
                let file_path = block.get("input")
                    .and_then(|i| i.get("file_path").or_else(|| i.get("path")))
                    .and_then(|p| p.as_str()).unwrap_or("").to_string();
                if !id.is_empty() && !file_path.is_empty() {
                    recent_tool_map.insert(id, file_path);
                }
            }
        }
    }
    for msg in messages[cutoff..].iter() {
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
        if role != "user" { continue; }
        let Some(blocks) = msg.get("content").and_then(|c| c.as_array()) else { continue; };
        for block in blocks {
            if block.get("type").and_then(|t| t.as_str()) != Some("tool_result") { continue; }
            let tool_use_id = block.get("tool_use_id").and_then(|i| i.as_str()).unwrap_or("").to_string();
            let Some(file_path) = recent_tool_map.get(&tool_use_id) else { continue; };
            let content = match block.get("content") {
                Some(Value::String(s)) => s.clone(),
                Some(Value::Array(arr)) => arr.iter()
                    .filter_map(|b| if b.get("type").and_then(|t| t.as_str()) == Some("text") { b.get("text").and_then(|t| t.as_str()).map(|s| s.to_string()) } else { None })
                    .collect::<Vec<_>>().join("\n"),
                _ => continue,
            };
            if content.len() >= 50 {
                cache.insert(file_path, &content); // Cache only, don't modify
            }
        }
    }

    if stats.cache_hits > 0 {
        info!(
            hits = stats.cache_hits,
            checked = stats.files_checked,
            saved_bytes = stats.bytes_saved,
            cache_size = cache.len(),
            "File cache applied"
        );
    }

    stats
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_cache_miss() {
        let cache = FileCache::new(10);
        match cache.check("/src/main.rs", "fn main() {}") {
            CacheResult::Miss => {} // expected
            _ => panic!("Expected Miss"),
        }
    }

    #[test]
    fn test_cache_hit_unchanged() {
        let mut cache = FileCache::new(10);
        cache.insert("/src/main.rs", "fn main() {}");
        match cache.check("/src/main.rs", "fn main() {}") {
            CacheResult::Unchanged { original_len } => {
                assert_eq!(original_len, 12);
            }
            _ => panic!("Expected Unchanged"),
        }
    }

    #[test]
    fn test_cache_hit_changed() {
        let mut cache = FileCache::new(10);
        cache.insert("/src/main.rs", "fn main() {}");
        match cache.check("/src/main.rs", "fn main() { println!(\"hello\"); }") {
            CacheResult::Changed => {} // expected
            _ => panic!("Expected Changed"),
        }
    }

    #[test]
    fn test_lru_eviction() {
        let mut cache = FileCache::new(3);
        cache.insert("/a.rs", "a");
        cache.insert("/b.rs", "b");
        cache.insert("/c.rs", "c");
        assert_eq!(cache.len(), 3);

        // Insert 4th — should evict /a.rs
        cache.insert("/d.rs", "d");
        assert_eq!(cache.len(), 3);
        assert!(matches!(cache.check("/a.rs", "a"), CacheResult::Miss));
        assert!(matches!(cache.check("/b.rs", "b"), CacheResult::Unchanged { .. }));
    }

    #[test]
    fn test_clear() {
        let mut cache = FileCache::new(10);
        cache.insert("/a.rs", "a");
        cache.insert("/b.rs", "b");
        assert_eq!(cache.len(), 2);
        cache.clear();
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn test_apply_to_body() {
        let mut cache = FileCache::new(100);
        let file_content = "use std::io;\nfn main() {\n    println!(\"hello world\");\n}\n// end of file\n";

        // Populate cache with initial read
        cache.insert("/src/main.rs", file_content);

        // Request with OLD read (first pair) and RECENT read (last pair).
        // Only the OLD read should be replaced — recent ones stay intact.
        // Need 6+ messages so that the first pair is outside the cutoff (last 2 skipped).
        let mut body = Some(json!({
            "messages": [
                // Old read (will be in cutoff range — eligible for replacement)
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "call_old", "name": "Read", "input": {"file_path": "/src/main.rs"}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "call_old", "content": file_content}
                ]},
                // Middle messages
                {"role": "assistant", "content": [{"type": "text", "text": "I see the file."}]},
                {"role": "user", "content": "Make a change"},
                // Recent read (last 2 messages — should NOT be modified)
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "call_new", "name": "Read", "input": {"file_path": "/src/main.rs"}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "call_new", "content": file_content}
                ]},
            ]
        }));
        let stats = apply_file_cache(&mut body, &mut cache);

        // Old read should be a cache hit
        assert!(stats.cache_hits >= 1);
        assert!(stats.bytes_saved > 0);

        // Verify OLD read was replaced with marker
        let messages = body.as_ref().unwrap()["messages"].as_array().unwrap();
        let old_result = &messages[1]["content"][0];
        let old_content = old_result["content"].as_str().unwrap();
        assert!(old_content.contains("File unchanged"), "Old read should be replaced: {}", old_content);

        // Verify RECENT read was NOT modified
        let new_result = &messages[5]["content"][0];
        let new_content = new_result["content"].as_str().unwrap();
        assert!(!new_content.contains("File unchanged"), "Recent read should NOT be replaced");
        assert!(new_content.contains("fn main"), "Recent read should have original content");
    }
}
