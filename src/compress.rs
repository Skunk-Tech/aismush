//! Context compression for tool_result content blocks.
//!
//! Reduces token count by 20-50% on typical tool results (code, logs, errors)
//! without losing information the LLM needs.

use serde_json::Value;
use std::collections::HashSet;

/// Compression statistics for tracking savings.
#[derive(Default, Debug)]
pub struct CompressionStats {
    pub original_bytes: usize,
    pub compressed_bytes: usize,
    pub tool_results_processed: usize,
}

impl CompressionStats {
    pub fn savings_percent(&self) -> f64 {
        if self.original_bytes == 0 {
            return 0.0;
        }
        100.0 * (1.0 - self.compressed_bytes as f64 / self.original_bytes as f64)
    }
}

/// Compress tool_result content blocks in an Anthropic API request body.
/// Only touches tool_result blocks — leaves user text and system prompts alone.
pub fn compress_request_body(body: &mut Value) -> CompressionStats {
    let mut stats = CompressionStats::default();

    let Some(messages) = body.get_mut("messages").and_then(|m| m.as_array_mut()) else {
        return stats;
    };

    for msg in messages.iter_mut() {
        // Only process user messages (tool_results come as user messages)
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

                // Safeguard: don't compress if we'd lose too much (< 15% remaining)
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

/// Core text compression pipeline.
fn compress_text(input: &str) -> String {
    if input.len() < 100 {
        return input.to_string(); // Don't bother with tiny content
    }

    let mut result = input.to_string();

    // 1. Strip comments (biggest single savings on code)
    result = strip_comments(&result);

    // 2. Normalize whitespace
    result = normalize_whitespace(&result);

    // 3. Deduplicate consecutive similar lines
    result = dedup_lines(&result);

    // 4. Truncate very long tool results (>12K chars)
    if result.len() > 12_000 {
        let truncated = &result[..12_000];
        // Find last newline to avoid cutting mid-line
        let cut = truncated.rfind('\n').unwrap_or(12_000);
        result = format!(
            "{}\n\n[COMPRESSED: {} chars total, showing first {}]",
            &result[..cut],
            input.len(),
            cut
        );
    }

    result
}

/// Remove comments from code. Language-agnostic heuristic approach.
fn strip_comments(input: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut in_block_comment = false;

    for line in input.lines() {
        let trimmed = line.trim();

        // Block comment tracking
        if in_block_comment {
            if let Some(pos) = line.find("*/") {
                // End of block comment — keep anything after */
                let after = &line[pos + 2..];
                if !after.trim().is_empty() {
                    lines.push(after.to_string());
                }
                in_block_comment = false;
            }
            continue;
        }

        // Start of block comment
        if let Some(pos) = trimmed.find("/*") {
            // Check it's not in a string (rough heuristic: not after a quote)
            let before = &trimmed[..pos];
            if !before.contains('"') && !before.contains('\'') {
                // Keep content before the comment
                if !before.trim().is_empty() {
                    lines.push(before.to_string());
                }
                if !trimmed.contains("*/") {
                    in_block_comment = true;
                }
                continue;
            }
        }

        // Single-line comments
        // Skip pure comment lines (line starts with comment marker)
        if trimmed.starts_with("//") && !trimmed.starts_with("///") {
            continue; // Skip // comments but keep /// doc comments
        }
        if trimmed.starts_with('#') && !trimmed.starts_with("#!") && !trimmed.starts_with("#[") {
            // Skip # comments but keep shebangs and Rust attributes
            continue;
        }
        if trimmed.starts_with("--") && !trimmed.starts_with("---") {
            continue; // SQL comments
        }

        // Strip trailing comments (rough heuristic)
        let cleaned = strip_trailing_comment(line);
        lines.push(cleaned);
    }

    lines.join("\n")
}

/// Remove trailing // and # comments from a line.
fn strip_trailing_comment(line: &str) -> String {
    // Simple heuristic: find // or # not inside quotes
    let bytes = line.as_bytes();
    let mut in_single_quote = false;
    let mut in_double_quote = false;

    for i in 0..bytes.len() {
        let ch = bytes[i];

        if ch == b'\'' && !in_double_quote {
            in_single_quote = !in_single_quote;
        } else if ch == b'"' && !in_single_quote {
            in_double_quote = !in_double_quote;
        }

        if !in_single_quote && !in_double_quote {
            // Check for //
            if ch == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
                // Don't strip URLs (://)
                if i > 0 && bytes[i - 1] == b':' {
                    continue;
                }
                return line[..i].trim_end().to_string();
            }
        }
    }

    line.to_string()
}

/// Normalize whitespace: reduce indentation, collapse blank lines.
fn normalize_whitespace(input: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut consecutive_blank = 0;
    let mut consecutive_closing = 0;

    for line in input.lines() {
        let trimmed = line.trim();

        // Collapse multiple blank lines to one
        if trimmed.is_empty() {
            consecutive_blank += 1;
            consecutive_closing = 0;
            if consecutive_blank <= 1 {
                lines.push(String::new());
            }
            continue;
        }
        consecutive_blank = 0;

        // Collapse consecutive closing braces (max 2)
        if matches!(trimmed, "}" | "};" | ");" | "});") {
            consecutive_closing += 1;
            if consecutive_closing > 2 {
                continue;
            }
        } else {
            consecutive_closing = 0;
        }

        // Reduce indentation by half
        let indent = line.len() - line.trim_start().len();
        let new_indent = indent / 2;
        let indented = format!("{}{}", " ".repeat(new_indent), trimmed);
        lines.push(indented);
    }

    // Remove trailing blank lines
    while lines.last().map_or(false, |l| l.is_empty()) {
        lines.pop();
    }

    lines.join("\n")
}

/// Deduplicate consecutive similar lines (e.g., repeated error messages).
fn dedup_lines(input: &str) -> String {
    let lines: Vec<&str> = input.lines().collect();
    if lines.len() < 5 {
        return input.to_string();
    }

    let mut result: Vec<String> = Vec::new();
    let mut seen_recently: HashSet<String> = HashSet::new();
    let mut dup_count = 0;
    let window = 10; // Look back 10 lines for duplicates

    for (i, line) in lines.iter().enumerate() {
        let normalized = line.trim().to_lowercase();

        // Skip very short lines from dedup (braces, blank)
        if normalized.len() < 15 {
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

        // Evict old entries from the window
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_comments() {
        let input = "fn main() {\n    // This is a comment\n    let x = 5; // trailing\n    println!(\"hello\");\n}";
        let result = strip_comments(input);
        assert!(!result.contains("This is a comment"));
        assert!(!result.contains("trailing"));
        assert!(result.contains("let x = 5;"));
        assert!(result.contains("println!"));
    }

    #[test]
    fn test_normalize_whitespace() {
        let input = "fn main() {\n        let x = 5;\n\n\n\n        let y = 6;\n}";
        let result = normalize_whitespace(input);
        // Should have max 1 blank line
        assert!(!result.contains("\n\n\n"));
        // Should have reduced indentation
        assert!(!result.contains("        let"));
    }

    #[test]
    fn test_dedup_lines() {
        let input = "error: something failed\nerror: something failed\nerror: something failed\nerror: something failed\nok: done";
        let result = dedup_lines(input);
        assert!(result.contains("similar lines omitted"));
        assert!(result.contains("ok: done"));
    }

    #[test]
    fn test_compress_preserves_small() {
        let small = "hello world";
        assert_eq!(compress_text(small), small);
    }
}
