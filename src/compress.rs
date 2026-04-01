//! Context compression for tool_result content blocks.
//!
//! Inspired by RTK's approach: language-aware filtering, data format protection,
//! smart truncation that preserves structural elements.

use serde_json::Value;
use std::collections::HashSet;

#[derive(Default, Debug)]
pub struct CompressionStats {
    pub original_bytes: usize,
    pub compressed_bytes: usize,
    pub tool_results_processed: usize,
}

impl CompressionStats {
    pub fn savings_percent(&self) -> f64 {
        if self.original_bytes == 0 { return 0.0; }
        100.0 * (1.0 - self.compressed_bytes as f64 / self.original_bytes as f64)
    }
}

/// Compress tool_result content blocks in an Anthropic API request body.
pub fn compress_request_body(body: &mut Value) -> CompressionStats {
    let mut stats = CompressionStats::default();

    let Some(messages) = body.get_mut("messages").and_then(|m| m.as_array_mut()) else {
        return stats;
    };

    for msg in messages.iter_mut() {
        if msg.get("role").and_then(|r| r.as_str()) != Some("user") {
            continue;
        }
        let Some(content) = msg.get_mut("content").and_then(|c| c.as_array_mut()) else {
            continue;
        };

        for block in content.iter_mut() {
            if block.get("type").and_then(|t| t.as_str()) != Some("tool_result") {
                continue;
            }

            stats.tool_results_processed += 1;

            // Handle string content
            if let Some(text) = block.get("content").and_then(|c| c.as_str()).map(|s| s.to_string()) {
                let original = text.len();
                let compressed = compress_text(&text);
                let compressed_len = compressed.len();

                // Safeguard: don't use compressed if it's too aggressive (< 10% remaining)
                if compressed_len > 0 && compressed_len as f64 / original as f64 > 0.10 {
                    stats.original_bytes += original;
                    stats.compressed_bytes += compressed_len;
                    block["content"] = Value::String(compressed);
                } else {
                    stats.original_bytes += original;
                    stats.compressed_bytes += original;
                }
            }

            // Handle array content (mixed text/image blocks)
            if let Some(arr) = block.get_mut("content").and_then(|c| c.as_array_mut()) {
                for item in arr.iter_mut() {
                    if item.get("type").and_then(|t| t.as_str()) == Some("text") {
                        if let Some(text) = item.get("text").and_then(|t| t.as_str()).map(|s| s.to_string()) {
                            let original = text.len();
                            let compressed = compress_text(&text);
                            let compressed_len = compressed.len();

                            if compressed_len > 0 && compressed_len as f64 / original as f64 > 0.10 {
                                stats.original_bytes += original;
                                stats.compressed_bytes += compressed_len;
                                item["text"] = Value::String(compressed);
                            } else {
                                stats.original_bytes += original;
                                stats.compressed_bytes += original;
                            }
                        }
                    }
                }
            }
        }
    }

    stats
}

// ── Content type detection ──────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
enum ContentType {
    Code,    // Source code — safe to strip comments
    Data,    // JSON, YAML, XML, CSV — NEVER modify
    Log,     // Log output, error traces — dedup but don't strip
    Unknown, // Default — conservative compression only
}

/// Detect what kind of content this is.
fn detect_content_type(text: &str) -> ContentType {
    let trimmed = text.trim();

    // JSON detection — starts with { or [ followed by JSON-like content
    if trimmed.starts_with('{') {
        return ContentType::Data;
    }
    if trimmed.starts_with('[') && (trimmed.starts_with("[{") || trimmed.starts_with("[\"") || trimmed.starts_with("[1") || trimmed.starts_with("[\n")) {
        return ContentType::Data;
    }

    // YAML detection — starts with --- or key: value
    if trimmed.starts_with("---") || (trimmed.contains(": ") && !trimmed.contains("//") && trimmed.lines().take(3).all(|l| l.contains(": ") || l.trim().is_empty() || l.starts_with('#'))) {
        return ContentType::Data;
    }

    // XML/HTML detection
    if trimmed.starts_with("<?xml") || trimmed.starts_with("<!DOCTYPE") || trimmed.starts_with("<html") {
        return ContentType::Data;
    }

    // Log detection — timestamps, repeated patterns
    let lines: Vec<&str> = trimmed.lines().take(5).collect();
    if lines.len() >= 3 {
        let log_patterns = lines.iter().filter(|l| {
            l.contains("[INFO]") || l.contains("[WARN]") || l.contains("[ERROR]") ||
            l.contains("[DEBUG]") || l.contains(" INFO ") || l.contains(" ERROR ") ||
            l.contains("at ") && l.contains(".rs:") || // Rust stack traces
            l.contains("at ") && (l.contains(".ts:") || l.contains(".js:")) // JS stack traces
        }).count();
        if log_patterns >= 2 {
            return ContentType::Log;
        }
    }

    // Code detection — look for language indicators
    let code_indicators = trimmed.lines().take(10).filter(|l| {
        let t = l.trim();
        t.starts_with("fn ") || t.starts_with("pub ") || t.starts_with("use ") ||
        t.starts_with("import ") || t.starts_with("from ") || t.starts_with("const ") ||
        t.starts_with("let ") || t.starts_with("def ") || t.starts_with("class ") ||
        t.starts_with("function ") || t.starts_with("export ") || t.starts_with("#include") ||
        t.starts_with("//") || t.starts_with("/*") || t.starts_with("#!")
    }).count();

    if code_indicators >= 2 {
        return ContentType::Code;
    }

    ContentType::Unknown
}

// ── Core compression ────────────────────────────────────────────────────────

fn compress_text(input: &str) -> String {
    if input.len() < 100 {
        return input.to_string();
    }

    let content_type = detect_content_type(input);

    match content_type {
        ContentType::Data => {
            // NEVER strip comments from data formats — just truncate if huge
            if input.len() > 12_000 {
                smart_truncate_data(input, 12_000)
            } else {
                input.to_string()
            }
        }
        ContentType::Code => {
            let mut result = strip_comments(input);
            result = normalize_whitespace(&result);
            result = dedup_lines(&result);
            if result.len() > 12_000 {
                result = smart_truncate_code(&result, 12_000);
            }
            result
        }
        ContentType::Log => {
            // Don't strip comments from logs, but dedup aggressively
            let mut result = dedup_lines(input);
            result = normalize_whitespace(&result);
            if result.len() > 8_000 {
                smart_truncate_data(&result, 8_000)
            } else {
                result
            }
        }
        ContentType::Unknown => {
            // Conservative — only whitespace normalization and dedup
            let mut result = normalize_whitespace(input);
            result = dedup_lines(&result);
            if result.len() > 12_000 {
                smart_truncate_data(&result, 12_000)
            } else {
                result
            }
        }
    }
}

// ── Comment stripping (code only) ───────────────────────────────────────────

fn strip_comments(input: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut in_block_comment = false;

    for line in input.lines() {
        let trimmed = line.trim();

        // Block comment tracking
        if in_block_comment {
            if let Some(pos) = line.find("*/") {
                let after = &line[pos + 2..];
                if !after.trim().is_empty() {
                    lines.push(after.to_string());
                }
                in_block_comment = false;
            }
            continue;
        }

        // Start of block comment (not in strings)
        if let Some(pos) = trimmed.find("/*") {
            let before = &trimmed[..pos];
            if !before.contains('"') && !before.contains('\'') {
                if !before.trim().is_empty() {
                    lines.push(before.to_string());
                }
                if !trimmed.contains("*/") {
                    in_block_comment = true;
                }
                continue;
            }
        }

        // Single-line comments — skip pure comment lines
        if trimmed.starts_with("//") && !trimmed.starts_with("///") {
            continue;
        }
        if trimmed.starts_with('#') && !trimmed.starts_with("#!") && !trimmed.starts_with("#[") {
            continue;
        }
        if trimmed.starts_with("--") && !trimmed.starts_with("---") {
            continue;
        }

        lines.push(line.to_string());
    }

    lines.join("\n")
}

// ── Whitespace normalization ────────────────────────────────────────────────

fn normalize_whitespace(input: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut consecutive_blank = 0;
    let mut consecutive_closing = 0;

    for line in input.lines() {
        let trimmed = line.trim();

        if trimmed.is_empty() {
            consecutive_blank += 1;
            consecutive_closing = 0;
            if consecutive_blank <= 1 {
                lines.push(String::new());
            }
            continue;
        }
        consecutive_blank = 0;

        if matches!(trimmed, "}" | "};" | ");" | "});") {
            consecutive_closing += 1;
            if consecutive_closing > 2 { continue; }
        } else {
            consecutive_closing = 0;
        }

        // Reduce indentation by half
        let indent = line.len() - line.trim_start().len();
        let new_indent = indent / 2;
        lines.push(format!("{}{}", " ".repeat(new_indent), trimmed));
    }

    while lines.last().map_or(false, |l| l.is_empty()) { lines.pop(); }
    lines.join("\n")
}

// ── Line deduplication ──────────────────────────────────────────────────────

fn dedup_lines(input: &str) -> String {
    let lines: Vec<&str> = input.lines().collect();
    if lines.len() < 5 { return input.to_string(); }

    let mut result: Vec<String> = Vec::new();
    let mut seen_recently: HashSet<String> = HashSet::new();
    let mut dup_count = 0;
    let window = 10;

    for (i, line) in lines.iter().enumerate() {
        let normalized = line.trim().to_lowercase();

        if normalized.len() < 15 {
            if dup_count > 0 {
                result.push(format!("  [... {} similar lines omitted]", dup_count));
                dup_count = 0;
            }
            result.push(line.to_string());
            seen_recently.insert(normalized);
            continue;
        }

        if seen_recently.contains(&normalized) {
            dup_count += 1;
            continue;
        }

        if dup_count > 0 {
            result.push(format!("  [... {} similar lines omitted]", dup_count));
            dup_count = 0;
        }

        result.push(line.to_string());
        seen_recently.insert(normalized);

        if i >= window {
            let old = lines[i - window].trim().to_lowercase();
            seen_recently.remove(&old);
        }
    }

    if dup_count > 0 {
        result.push(format!("  [... {} similar lines omitted]", dup_count));
    }

    result.join("\n")
}

// ── Smart truncation ────────────────────────────────────────────────────────

/// Truncate code preserving function signatures and imports (RTK-inspired).
fn smart_truncate_code(input: &str, max_chars: usize) -> String {
    let lines: Vec<&str> = input.lines().collect();
    let mut result: Vec<&str> = Vec::new();
    let mut char_count = 0;

    for line in &lines {
        let is_important = {
            let t = line.trim();
            t.starts_with("fn ") || t.starts_with("pub ") || t.starts_with("use ") ||
            t.starts_with("import ") || t.starts_with("from ") || t.starts_with("export ") ||
            t.starts_with("class ") || t.starts_with("def ") || t.starts_with("function ") ||
            t.starts_with("struct ") || t.starts_with("enum ") || t.starts_with("trait ") ||
            t.starts_with("interface ") || t.starts_with("type ") ||
            t == "}" || t == "{" || t.starts_with("const ") || t.starts_with("let ")
        };

        if char_count < max_chars / 2 || is_important {
            result.push(line);
            char_count += line.len() + 1;
        }

        if char_count >= max_chars && !is_important {
            break;
        }
    }

    if result.len() < lines.len() {
        let remaining = lines.len() - result.len();
        result.push(&"");
        let msg = format!("[... {} more lines, {} total]", remaining, lines.len());
        // Can't push owned string to &str vec, so format inline
        return format!("{}\n[... {} more lines, {} total]", result.join("\n"), remaining, lines.len());
    }

    result.join("\n")
}

/// Truncate data formats (JSON, YAML, logs) — simple cutoff at line boundary.
fn smart_truncate_data(input: &str, max_chars: usize) -> String {
    if input.len() <= max_chars {
        return input.to_string();
    }
    let truncated = &input[..max_chars];
    let cut = truncated.rfind('\n').unwrap_or(max_chars);
    format!("{}\n[... {} chars total, showing first {}]", &input[..cut], input.len(), cut)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_json_not_corrupted() {
        let json = r#"{"workspaces":{"packages":["packages/*"]},"scripts":{"build":"bun run build"}}"#;
        let result = compress_text(json);
        assert_eq!(result, json, "JSON must pass through unchanged");
    }

    #[test]
    fn test_yaml_not_corrupted() {
        let yaml = "---\nname: test\nversion: 1.0\ndependencies:\n  react: ^18.0.0\n";
        let result = compress_text(yaml);
        assert!(result.contains("react: ^18.0.0"), "YAML must not be modified");
    }

    #[test]
    fn test_code_comments_stripped() {
        // Need enough code indicators to be detected as Code
        let code = "use std::io;\nimport something;\n// This is a comment\nfn main() {\n    // inline comment\n    let x = 5;\n    let y = 10;\n    println!(\"hello\");\n}\n// trailing comment\nexport default App;\n";
        let result = compress_text(code);
        assert!(!result.contains("This is a comment"), "Comment should be stripped: {}", result);
        assert!(result.contains("fn main()"));
        assert!(result.contains("let x = 5"));
    }

    #[test]
    fn test_log_deduped() {
        assert_eq!(detect_content_type("[INFO] Starting\n[INFO] Step 1\n[ERROR] Failed\n"), ContentType::Log);
        // Use enough lines and content for dedup to work
        let log = "[INFO] Server starting up now\n[INFO] Connected to database ok\n[INFO] Processing item number 1\n[INFO] Processing item number 1\n[INFO] Processing item number 1\n[INFO] Processing item number 1\n[INFO] Processing item number 1\n[INFO] Processing item number 1\n[INFO] Server done processing\n";
        let result = compress_text(log);
        assert!(result.contains("similar lines omitted"), "Should dedup repeated lines: {}", result);
        assert!(result.contains("[INFO] Server done processing"));
    }

    #[test]
    fn test_small_content_unchanged() {
        let small = "hello world";
        assert_eq!(compress_text(small), small);
    }

    #[test]
    fn test_detect_json() {
        assert_eq!(detect_content_type(r#"{"key":"value"}"#), ContentType::Data);
        assert_eq!(detect_content_type(r#"[1, 2, 3]"#), ContentType::Data);
    }

    #[test]
    fn test_detect_code() {
        assert_eq!(detect_content_type("use std::io;\nfn main() {\n}\n"), ContentType::Code);
        assert_eq!(detect_content_type("import React from 'react';\nconst App = () => {};\n"), ContentType::Code);
    }
}
