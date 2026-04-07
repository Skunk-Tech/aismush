//! Command-specific compression patterns for CLI output.
//!
//! Detects common command output (cargo, git, npm, docker, test runners)
//! and applies targeted compression that preserves essential information
//! while stripping boilerplate, progress indicators, and noise.

/// Try to compress CLI output using command-specific patterns.
/// Returns `Some(compressed)` if a pattern matched, `None` if unrecognized.
pub fn compress_command_output(text: &str) -> Option<String> {
    let cleaned = strip_ansi(text);
    let cleaned = strip_progress_bars(&cleaned);

    match detect_command(&cleaned) {
        Command::CargoTest => Some(compress_cargo_test(&cleaned)),
        Command::CargoBuild => Some(compress_cargo_build(&cleaned)),
        Command::GitStatus => Some(compress_git_status(&cleaned)),
        Command::GitDiff => Some(compress_git_diff(&cleaned)),
        Command::GitLog => Some(compress_git_log(&cleaned)),
        Command::TestRunner => Some(compress_test_runner(&cleaned)),
        Command::NpmOutput => Some(compress_npm_output(&cleaned)),
        Command::DockerOutput => Some(compress_docker_output(&cleaned)),
        Command::Unknown => None,
    }
}

#[derive(Debug, PartialEq)]
enum Command {
    CargoTest,
    CargoBuild,
    GitStatus,
    GitDiff,
    GitLog,
    TestRunner,
    NpmOutput,
    DockerOutput,
    Unknown,
}

fn detect_command(text: &str) -> Command {
    let lines: Vec<&str> = text.lines().take(15).collect();
    let text_lower = text.to_lowercase();

    // Cargo test: "running N tests" or "test result:"
    if lines.iter().any(|l| {
        let lt = l.trim();
        (lt.contains("running") && lt.contains("test")) || lt.starts_with("test result:")
    }) {
        return Command::CargoTest;
    }

    // Cargo build: "Compiling" + "Finished" or "error[E"
    let has_compiling = lines.iter().any(|l| l.trim().starts_with("Compiling") || l.trim().starts_with("Downloading"));
    let has_finished = text_lower.contains("finished");
    let has_cargo_error = text.contains("error[E") || text.contains("error: could not compile");
    if has_compiling && (has_finished || has_cargo_error) {
        return Command::CargoBuild;
    }
    // Also catch cargo check/clippy with just errors
    if has_cargo_error {
        return Command::CargoBuild;
    }

    // Git diff: starts with "diff --git"
    if lines.first().map_or(false, |l| l.starts_with("diff --git")) {
        return Command::GitDiff;
    }

    // Git log: "commit " followed by hex hash
    if lines.first().map_or(false, |l| {
        l.starts_with("commit ") && l.len() > 14 && l[7..].chars().take(7).all(|c| c.is_ascii_hexdigit())
    }) {
        return Command::GitLog;
    }

    // Git status: "On branch" or specific status markers
    if lines.first().map_or(false, |l| l.starts_with("On branch")) ||
       lines.iter().any(|l| l.contains("Changes not staged") || l.contains("Changes to be committed")) {
        return Command::GitStatus;
    }

    // Test runners (jest, pytest, vitest, mocha)
    if text_lower.contains("test suites:") || text_lower.contains("tests passed") ||
       text_lower.contains("passed,") && text_lower.contains("failed") ||
       lines.iter().any(|l| l.contains("PASS ") || l.contains("FAIL ") || l.contains("✓") || l.contains("✗")) {
        return Command::TestRunner;
    }

    // npm/yarn/pnpm
    if lines.iter().any(|l| {
        let lt = l.to_lowercase();
        lt.contains("npm") || lt.contains("yarn") || lt.contains("pnpm")
    }) && text_lower.contains("added") || text_lower.contains("npm err!") || text_lower.contains("npm warn") {
        return Command::NpmOutput;
    }

    // Docker
    if lines.first().map_or(false, |l| l.contains("CONTAINER ID") || l.contains("REPOSITORY")) ||
       lines.iter().any(|l| l.starts_with("docker")) {
        return Command::DockerOutput;
    }

    Command::Unknown
}

fn compress_cargo_test(text: &str) -> String {
    let mut result = String::new();
    let mut total_tests = 0u32;
    let mut passed = 0u32;
    let mut failed_names: Vec<String> = Vec::new();
    let mut in_failure_detail = false;
    let mut failure_detail = String::new();
    let mut result_line = String::new();

    for line in text.lines() {
        let trimmed = line.trim();

        // Skip build/compilation lines
        if trimmed.starts_with("Compiling") || trimmed.starts_with("Downloading") ||
           trimmed.starts_with("Finished") || trimmed.starts_with("Running") ||
           trimmed.is_empty() {
            continue;
        }

        // Count running tests
        if trimmed.starts_with("running") && trimmed.contains("test") {
            if let Some(n) = trimmed.split_whitespace().nth(1).and_then(|s| s.parse::<u32>().ok()) {
                total_tests += n;
            }
            continue;
        }

        // Track individual test results
        if trimmed.starts_with("test ") && trimmed.contains(" ... ") {
            if trimmed.ends_with("ok") {
                passed += 1;
            } else if trimmed.contains("FAILED") {
                let name = trimmed.strip_prefix("test ").unwrap_or(trimmed)
                    .split(" ... ").next().unwrap_or("unknown");
                failed_names.push(name.to_string());
            }
            continue;
        }

        // Capture failure details
        if trimmed.starts_with("---- ") && trimmed.ends_with(" ----") {
            in_failure_detail = true;
            failure_detail.push_str(line);
            failure_detail.push('\n');
            continue;
        }
        if in_failure_detail {
            if trimmed.starts_with("---- ") || trimmed == "failures:" || trimmed.starts_with("test result:") {
                in_failure_detail = false;
            } else {
                failure_detail.push_str(line);
                failure_detail.push('\n');
                continue;
            }
        }

        // Capture the result line
        if trimmed.starts_with("test result:") {
            result_line = trimmed.to_string();
            continue;
        }
    }

    // Build compressed output
    if total_tests == 0 { total_tests = passed + failed_names.len() as u32; }

    if failed_names.is_empty() {
        result.push_str(&format!("running {} tests — all passed\n", total_tests));
    } else {
        result.push_str(&format!("running {} tests — {} FAILED\n", total_tests, failed_names.len()));
        for name in &failed_names {
            result.push_str(&format!("FAIL: {}\n", name));
        }
        if !failure_detail.is_empty() {
            result.push_str(&failure_detail);
        }
    }

    if !result_line.is_empty() {
        result.push_str(&result_line);
        result.push('\n');
    }

    result
}

fn compress_cargo_build(text: &str) -> String {
    let mut result = String::new();
    let mut seen_warnings: Vec<String> = Vec::new();

    for line in text.lines() {
        let trimmed = line.trim();

        // Skip compilation progress
        if trimmed.starts_with("Compiling") || trimmed.starts_with("Downloading") ||
           trimmed.starts_with("Blocking") || trimmed.starts_with("Updating") ||
           trimmed.is_empty() {
            continue;
        }

        // Keep errors (always)
        if trimmed.contains("error[E") || trimmed.contains("error:") || trimmed.contains("error: could not compile") {
            result.push_str(line);
            result.push('\n');
            continue;
        }

        // Keep warnings (deduplicated)
        if trimmed.contains("warning:") || trimmed.contains("warning[") {
            let msg = trimmed.split("warning").nth(1).unwrap_or(trimmed);
            if !seen_warnings.iter().any(|w| w == msg) {
                seen_warnings.push(msg.to_string());
                result.push_str(line);
                result.push('\n');
            }
            continue;
        }

        // Keep context lines for errors (lines with --> or | that follow errors)
        if trimmed.starts_with("-->") || (trimmed.starts_with('|') && !result.is_empty()) ||
           trimmed.starts_with("= help:") || trimmed.starts_with("= note:") {
            result.push_str(line);
            result.push('\n');
            continue;
        }

        // Keep the Finished line
        if trimmed.starts_with("Finished") {
            result.push_str(trimmed);
            result.push('\n');
            continue;
        }

        // Keep "generated N warnings" summary
        if trimmed.contains("generated") && trimmed.contains("warning") {
            result.push_str(trimmed);
            result.push('\n');
        }
    }

    if result.is_empty() {
        // If nothing interesting, return a minimal summary
        "build completed (no errors or warnings)\n".to_string()
    } else {
        result
    }
}

fn compress_git_status(text: &str) -> String {
    let mut branch = String::new();
    let mut modified: Vec<String> = Vec::new();
    let mut added: Vec<String> = Vec::new();
    let mut deleted: Vec<String> = Vec::new();
    let mut untracked: Vec<String> = Vec::new();
    let mut staged: Vec<String> = Vec::new();

    for line in text.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with("On branch ") {
            branch = trimmed[10..].to_string();
            continue;
        }

        // Skip hint lines
        if trimmed.starts_with("(use \"git") || trimmed.starts_with("(use 'git") || trimmed.is_empty() {
            continue;
        }

        // Skip section headers
        if trimmed.starts_with("Changes not staged") || trimmed.starts_with("Changes to be committed") ||
           trimmed.starts_with("Untracked files:") || trimmed.starts_with("Your branch") {
            if trimmed.starts_with("Changes to be committed") {
                // Next files are staged
            }
            continue;
        }

        // Parse file entries
        if trimmed.starts_with("modified:") {
            let file = trimmed.strip_prefix("modified:").unwrap_or("").trim();
            modified.push(file.to_string());
        } else if trimmed.starts_with("new file:") {
            let file = trimmed.strip_prefix("new file:").unwrap_or("").trim();
            staged.push(file.to_string());
        } else if trimmed.starts_with("deleted:") {
            let file = trimmed.strip_prefix("deleted:").unwrap_or("").trim();
            deleted.push(file.to_string());
        } else if trimmed.starts_with("renamed:") {
            let file = trimmed.strip_prefix("renamed:").unwrap_or("").trim();
            modified.push(file.to_string());
        } else if !trimmed.is_empty() && !trimmed.starts_with('#') && !trimmed.starts_with("no changes") {
            // Untracked files or short-format entries
            if line.starts_with("??") || line.starts_with("\t") {
                let file = trimmed.trim_start_matches("?? ");
                untracked.push(file.to_string());
            }
        }
    }

    let mut result = String::new();
    if !branch.is_empty() {
        result.push_str(&format!("branch: {}\n", branch));
    }
    if !staged.is_empty() {
        result.push_str(&format!("staged: {}\n", staged.join(", ")));
    }
    if !modified.is_empty() {
        result.push_str(&format!("modified: {}\n", modified.join(", ")));
    }
    if !deleted.is_empty() {
        result.push_str(&format!("deleted: {}\n", deleted.join(", ")));
    }
    if !untracked.is_empty() {
        result.push_str(&format!("untracked: {}\n", untracked.join(", ")));
    }
    if result.is_empty() {
        "clean working tree\n".to_string()
    } else {
        result
    }
}

fn compress_git_diff(text: &str) -> String {
    let mut result = String::new();
    let mut current_file = String::new();

    for line in text.lines() {
        // Extract file path from diff header
        if line.starts_with("diff --git") {
            if let Some(path) = line.split(" b/").last() {
                current_file = path.to_string();
                result.push_str(&format!("--- {} ---\n", current_file));
            }
            continue;
        }

        // Skip redundant headers
        if line.starts_with("index ") || line.starts_with("--- ") || line.starts_with("+++ ") ||
           line.starts_with("old mode") || line.starts_with("new mode") {
            continue;
        }

        // Keep hunk headers
        if line.starts_with("@@") {
            result.push_str(line);
            result.push('\n');
            continue;
        }

        // Keep changed lines and minimal context
        if line.starts_with('+') || line.starts_with('-') || line.starts_with(' ') {
            result.push_str(line);
            result.push('\n');
        }

        // Keep binary file notices
        if line.starts_with("Binary files") {
            result.push_str(line);
            result.push('\n');
        }
    }

    result
}

fn compress_git_log(text: &str) -> String {
    let mut result = String::new();
    let mut current_hash = String::new();
    let mut current_date = String::new();
    let mut current_message = String::new();

    for line in text.lines() {
        let trimmed = line.trim();

        if line.starts_with("commit ") {
            // Flush previous entry
            if !current_hash.is_empty() {
                result.push_str(&format_log_entry(&current_hash, &current_message, &current_date));
            }
            // Extract short hash
            let hash_part = line[7..].split_whitespace().next().unwrap_or("");
            current_hash = hash_part.chars().take(7).collect();
            current_date.clear();
            current_message.clear();
            continue;
        }

        if trimmed.starts_with("Date:") {
            current_date = trimmed[5..].trim().to_string();
            // Try to shorten the date
            if current_date.len() > 16 {
                current_date = current_date.chars().take(16).collect::<String>().trim().to_string();
            }
            continue;
        }

        // Skip author line and merge info
        if trimmed.starts_with("Author:") || trimmed.starts_with("Merge:") {
            continue;
        }

        // Capture first non-empty message line
        if !trimmed.is_empty() && current_message.is_empty() && !current_hash.is_empty() {
            current_message = trimmed.to_string();
        }
    }

    // Flush last entry
    if !current_hash.is_empty() {
        result.push_str(&format_log_entry(&current_hash, &current_message, &current_date));
    }

    result
}

fn format_log_entry(hash: &str, message: &str, date: &str) -> String {
    if date.is_empty() {
        format!("{} {}\n", hash, message)
    } else {
        format!("{} {} ({})\n", hash, message, date)
    }
}

fn compress_test_runner(text: &str) -> String {
    let mut result = String::new();
    let mut failures: Vec<String> = Vec::new();

    for line in text.lines() {
        let trimmed = line.trim();
        let lower = trimmed.to_lowercase();

        // Keep summary lines
        if lower.contains("test suites:") || lower.contains("tests:") ||
           (lower.contains("passed") && lower.contains("failed")) ||
           lower.contains("test result") {
            result.push_str(trimmed);
            result.push('\n');
            continue;
        }

        // Keep failure details
        if trimmed.contains("FAIL") || trimmed.contains("✗") || trimmed.contains("✕") ||
           lower.contains("error") || lower.contains("expected") || lower.contains("received") {
            failures.push(trimmed.to_string());
        }
    }

    if !failures.is_empty() {
        for f in &failures {
            result.push_str(f);
            result.push('\n');
        }
    }

    if result.is_empty() {
        "tests completed\n".to_string()
    } else {
        result
    }
}

fn compress_npm_output(text: &str) -> String {
    let mut result = String::new();

    for line in text.lines() {
        let trimmed = line.trim();
        let lower = trimmed.to_lowercase();

        // Keep errors
        if lower.contains("err!") || lower.contains("error") {
            result.push_str(trimmed);
            result.push('\n');
            continue;
        }

        // Keep audit summary
        if lower.contains("vulnerabilities") || lower.contains("audit") {
            result.push_str(trimmed);
            result.push('\n');
            continue;
        }

        // Keep final status ("added N packages")
        if lower.contains("added") && lower.contains("packages") {
            result.push_str(trimmed);
            result.push('\n');
            continue;
        }

        // Keep "up to date" messages
        if lower.contains("up to date") {
            result.push_str(trimmed);
            result.push('\n');
        }
    }

    if result.is_empty() {
        "npm operation completed\n".to_string()
    } else {
        result
    }
}

fn compress_docker_output(text: &str) -> String {
    let mut result = String::new();

    for line in text.lines() {
        let trimmed = line.trim();

        // Skip empty lines
        if trimmed.is_empty() { continue; }

        // Skip build progress ("Step N/M" without errors)
        if trimmed.starts_with("Step ") && !trimmed.contains("error") && !trimmed.contains("Error") {
            continue;
        }

        // Skip pull progress
        if trimmed.contains("Pulling") || trimmed.contains("Waiting") ||
           trimmed.contains("Downloading") || trimmed.contains("Extracting") {
            continue;
        }

        // Truncate long SHA hashes to 12 chars
        let mut processed = trimmed.to_string();
        // Simple approach: find 64-char hex strings and shorten them
        if processed.len() > 64 {
            let mut chars: Vec<char> = processed.chars().collect();
            let mut i = 0;
            while i + 64 <= chars.len() {
                if chars[i..i+64].iter().all(|c| c.is_ascii_hexdigit()) {
                    // Replace with first 12 chars
                    let short: Vec<char> = chars[i..i+12].to_vec();
                    chars.splice(i..i+64, short);
                }
                i += 1;
            }
            processed = chars.into_iter().collect();
        }

        result.push_str(&processed);
        result.push('\n');
    }

    if result.is_empty() {
        "docker operation completed\n".to_string()
    } else {
        result
    }
}

/// Strip ANSI escape codes from text.
pub fn strip_ansi(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut in_escape = false;

    for c in text.chars() {
        if c == '\x1b' {
            in_escape = true;
            continue;
        }
        if in_escape {
            if c.is_ascii_alphabetic() {
                in_escape = false; // End of escape sequence
            }
            continue;
        }
        result.push(c);
    }

    result
}

/// Strip progress bar lines.
fn strip_progress_bars(text: &str) -> String {
    text.lines()
        .filter(|line| {
            let trimmed = line.trim();
            // Remove lines that look like progress bars
            !(trimmed.contains("░") || trimmed.contains("█") || trimmed.contains("[====") ||
              trimmed.contains("━") || trimmed.contains("▓") ||
              // Remove carriage-return-based progress (same-line updates)
              trimmed.contains('\r'))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_cargo_test() {
        let text = "running 5 tests\ntest foo ... ok\ntest result: ok.";
        assert_eq!(detect_command(text), Command::CargoTest);
    }

    #[test]
    fn test_detect_cargo_build() {
        let text = "   Compiling aismush v0.7.1\n    Finished release in 2m";
        assert_eq!(detect_command(text), Command::CargoBuild);
    }

    #[test]
    fn test_detect_git_status() {
        let text = "On branch main\nChanges not staged for commit:";
        assert_eq!(detect_command(text), Command::GitStatus);
    }

    #[test]
    fn test_detect_git_diff() {
        let text = "diff --git a/src/main.rs b/src/main.rs\nindex abc..def";
        assert_eq!(detect_command(text), Command::GitDiff);
    }

    #[test]
    fn test_detect_git_log() {
        let text = "commit dac113bfoobarfoobarfoobarfoobarfoobar12\nAuthor: Test";
        assert_eq!(detect_command(text), Command::GitLog);
    }

    #[test]
    fn test_detect_unknown() {
        let text = "hello world\nthis is just some text\nnothing special";
        assert_eq!(detect_command(text), Command::Unknown);
    }

    #[test]
    fn test_compress_cargo_test_all_pass() {
        let text = "\
   Compiling aismush v0.7.1
    Finished `test` profile in 1.64s
     Running unittests src/main.rs

running 3 tests
test compress::tests::test_a ... ok
test compress::tests::test_b ... ok
test compress::tests::test_c ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out";
        let compressed = compress_cargo_test(text);
        assert!(compressed.contains("all passed"));
        assert!(compressed.contains("test result:"));
        assert!(!compressed.contains("Compiling"));
        assert!(!compressed.contains("test_a ... ok"));
    }

    #[test]
    fn test_compress_cargo_test_failures() {
        let text = "\
running 3 tests
test foo ... ok
test bar ... FAILED
test baz ... ok

test result: FAILED. 2 passed; 1 failed";
        let compressed = compress_cargo_test(text);
        assert!(compressed.contains("FAILED"));
        assert!(compressed.contains("bar"));
        assert!(compressed.contains("test result:"));
    }

    #[test]
    fn test_compress_cargo_build_clean() {
        let text = "\
   Compiling serde v1.0.0
   Compiling tokio v1.0.0
   Compiling aismush v0.7.1
    Finished release in 2m";
        let compressed = compress_cargo_build(text);
        assert!(!compressed.contains("Compiling"));
        assert!(compressed.contains("Finished"));
    }

    #[test]
    fn test_compress_cargo_build_errors() {
        let text = "\
   Compiling aismush v0.7.1
error[E0308]: mismatched types
  --> src/main.rs:10:5
   |
10 |     let x: u32 = \"hello\";
   |                  ^^^^^^^ expected `u32`, found `&str`

error: could not compile `aismush`";
        let compressed = compress_cargo_build(text);
        assert!(compressed.contains("error[E0308]"));
        assert!(compressed.contains("src/main.rs:10"));
        assert!(!compressed.contains("Compiling"));
    }

    #[test]
    fn test_compress_git_status() {
        let text = "\
On branch main
Your branch is up to date with 'origin/main'.

Changes not staged for commit:
  (use \"git add <file>...\" to update what will be committed)
	modified:   src/main.rs
	modified:   src/lib.rs

Untracked files:
  (use \"git add <file>...\" to include in what will be committed)
	src/new_file.rs";
        let compressed = compress_git_status(text);
        assert!(compressed.contains("branch: main"));
        assert!(compressed.contains("src/main.rs"));
        assert!(compressed.contains("src/lib.rs"));
        assert!(!compressed.contains("use \"git add"));
    }

    #[test]
    fn test_compress_git_log() {
        let text = "\
commit dac113b1234567890abcdef1234567890abcdef (HEAD -> main)
Author: Test User <test@example.com>
Date:   Sat Apr 5 2026

    v0.7.1: Fix Plan Orchestrator

commit 540094e1234567890abcdef1234567890abcdef
Author: Test User <test@example.com>
Date:   Fri Apr 4 2026

    v0.7.0: Multi-provider routing";
        let compressed = compress_git_log(text);
        assert!(compressed.contains("dac113b"));
        assert!(compressed.contains("v0.7.1: Fix Plan Orchestrator"));
        assert!(compressed.contains("540094e"));
        assert!(!compressed.contains("Author:"));
    }

    #[test]
    fn test_compress_git_diff() {
        let text = "\
diff --git a/src/main.rs b/src/main.rs
index abc1234..def5678 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -10,3 +10,5 @@
 unchanged
-old line
+new line
+added line
 unchanged";
        let compressed = compress_git_diff(text);
        assert!(compressed.contains("src/main.rs"));
        assert!(compressed.contains("@@ -10,3 +10,5 @@"));
        assert!(compressed.contains("+new line"));
        assert!(compressed.contains("-old line"));
        assert!(!compressed.contains("index abc"));
        assert!(!compressed.contains("--- a/"));
    }

    #[test]
    fn test_strip_ansi() {
        let text = "\x1b[32mPASS\x1b[0m test_foo";
        assert_eq!(strip_ansi(text), "PASS test_foo");
    }

    #[test]
    fn test_unknown_returns_none() {
        let text = "just some random output\nnothing special here";
        assert!(compress_command_output(text).is_none());
    }
}
