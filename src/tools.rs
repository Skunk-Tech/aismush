use serde_json::Value;
use std::collections::HashMap;

/// Semantic category of a tool, independent of its name.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolCategory {
    FileRead,
    FileWrite,
    Shell,
    Search,
    Other(String),
}

impl ToolCategory {
    /// Human-readable observation category for memory system.
    pub fn observation_category(&self) -> &str {
        match self {
            ToolCategory::FileRead | ToolCategory::Search => "exploration",
            ToolCategory::FileWrite => "modification",
            ToolCategory::Shell => "command",
            ToolCategory::Other(_) => "tool",
        }
    }
}

/// Classifies tool calls by behavior rather than hardcoded names.
/// Uses three tiers: config overrides > smart defaults > input pattern detection.
pub struct ToolClassifier {
    name_map: HashMap<String, ToolCategory>,
}

impl ToolClassifier {
    /// Build a classifier from optional config overrides.
    pub fn new(mappings: Option<&ToolMappings>) -> Self {
        let mut name_map = HashMap::new();

        // Smart defaults — common tool names across Claude Code, Goose, and other agents
        for name in &["Read", "read", "read_file", "cat", "view", "get_file", "file_content", "view_file", "get_file_contents"] {
            name_map.insert(name.to_string(), ToolCategory::FileRead);
        }
        for name in &["Edit", "edit", "Write", "write", "write_file", "create_file", "patch", "patch_file", "replace_in_file"] {
            name_map.insert(name.to_string(), ToolCategory::FileWrite);
        }
        for name in &["Bash", "bash", "shell", "execute_command", "run_command", "run", "terminal", "exec"] {
            name_map.insert(name.to_string(), ToolCategory::Shell);
        }
        for name in &["Grep", "grep", "Glob", "glob", "search", "find_files", "ripgrep", "rg", "find"] {
            name_map.insert(name.to_string(), ToolCategory::Search);
        }

        // Config overrides (take priority over defaults)
        if let Some(m) = mappings {
            for name in &m.file_read {
                name_map.insert(name.clone(), ToolCategory::FileRead);
            }
            for name in &m.file_write {
                name_map.insert(name.clone(), ToolCategory::FileWrite);
            }
            for name in &m.shell {
                name_map.insert(name.clone(), ToolCategory::Shell);
            }
            for name in &m.search {
                name_map.insert(name.clone(), ToolCategory::Search);
            }
        }

        Self { name_map }
    }

    /// Classify a tool by name and optionally by its input structure.
    pub fn classify(&self, tool_name: &str, tool_input: Option<&Value>) -> ToolCategory {
        // Tier 1: exact name match
        if let Some(cat) = self.name_map.get(tool_name) {
            return cat.clone();
        }

        // Tier 2: substring matching on lowercased name
        let lower = tool_name.to_lowercase();
        if lower.contains("read") || lower.contains("view") || lower.contains("cat") {
            return ToolCategory::FileRead;
        }
        if lower.contains("write") || lower.contains("edit") || lower.contains("patch")
            || lower.contains("create_file") || lower.contains("replace") {
            return ToolCategory::FileWrite;
        }
        if lower.contains("bash") || lower.contains("shell") || lower.contains("exec")
            || lower.contains("command") || lower.contains("terminal") {
            return ToolCategory::Shell;
        }
        if lower.contains("grep") || lower.contains("search") || lower.contains("find")
            || lower.contains("glob") || lower.contains("ripgrep") {
            return ToolCategory::Search;
        }

        // Tier 3: input pattern detection
        if let Some(input) = tool_input {
            let has_path = input.get("file_path").is_some()
                || input.get("path").is_some()
                || input.get("filename").is_some()
                || input.get("file").is_some();
            let has_write_content = input.get("content").is_some()
                || input.get("new_string").is_some()
                || input.get("new_content").is_some();

            if has_path && has_write_content {
                return ToolCategory::FileWrite;
            }
            if has_path {
                return ToolCategory::FileRead;
            }
            if input.get("command").is_some() || input.get("cmd").is_some() {
                return ToolCategory::Shell;
            }
            if input.get("pattern").is_some() || input.get("query").is_some()
                || input.get("regex").is_some() {
                return ToolCategory::Search;
            }
        }

        ToolCategory::Other(tool_name.to_string())
    }

    /// Extract file path from tool input, trying multiple field name variants.
    pub fn extract_file_path<'a>(&self, input: &'a Value) -> Option<&'a str> {
        input.get("file_path")
            .or_else(|| input.get("path"))
            .or_else(|| input.get("filename"))
            .or_else(|| input.get("file"))
            .and_then(|v| v.as_str())
    }

    /// Extract command from tool input.
    pub fn extract_command<'a>(&self, input: &'a Value) -> Option<&'a str> {
        input.get("command")
            .or_else(|| input.get("cmd"))
            .and_then(|v| v.as_str())
    }

    /// Extract search pattern from tool input.
    pub fn extract_pattern<'a>(&self, input: &'a Value) -> Option<&'a str> {
        input.get("pattern")
            .or_else(|| input.get("query"))
            .or_else(|| input.get("regex"))
            .and_then(|v| v.as_str())
    }
}

impl Default for ToolClassifier {
    fn default() -> Self {
        Self::new(None)
    }
}

/// User-configurable tool name mappings.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
pub struct ToolMappings {
    #[serde(alias = "fileRead")]
    pub file_read: Vec<String>,
    #[serde(alias = "fileWrite")]
    pub file_write: Vec<String>,
    pub shell: Vec<String>,
    pub search: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classifier() -> ToolClassifier {
        ToolClassifier::new(None)
    }

    #[test]
    fn test_claude_code_tools() {
        let c = classifier();
        assert_eq!(c.classify("Read", None), ToolCategory::FileRead);
        assert_eq!(c.classify("Edit", None), ToolCategory::FileWrite);
        assert_eq!(c.classify("Write", None), ToolCategory::FileWrite);
        assert_eq!(c.classify("Bash", None), ToolCategory::Shell);
        assert_eq!(c.classify("Grep", None), ToolCategory::Search);
        assert_eq!(c.classify("Glob", None), ToolCategory::Search);
    }

    #[test]
    fn test_goose_style_tools() {
        let c = classifier();
        assert_eq!(c.classify("read_file", None), ToolCategory::FileRead);
        assert_eq!(c.classify("write_file", None), ToolCategory::FileWrite);
        assert_eq!(c.classify("execute_command", None), ToolCategory::Shell);
    }

    #[test]
    fn test_substring_matching() {
        let c = classifier();
        assert_eq!(c.classify("my_file_reader", None), ToolCategory::FileRead);
        assert_eq!(c.classify("run_shell_command", None), ToolCategory::Shell);
        assert_eq!(c.classify("code_search", None), ToolCategory::Search);
    }

    #[test]
    fn test_input_pattern_detection() {
        let c = classifier();

        let file_input = serde_json::json!({"file_path": "/src/main.rs"});
        assert_eq!(c.classify("custom_tool", Some(&file_input)), ToolCategory::FileRead);

        let write_input = serde_json::json!({"path": "/src/main.rs", "content": "fn main() {}"});
        assert_eq!(c.classify("custom_tool", Some(&write_input)), ToolCategory::FileWrite);

        let cmd_input = serde_json::json!({"command": "ls -la"});
        assert_eq!(c.classify("custom_tool", Some(&cmd_input)), ToolCategory::Shell);

        let search_input = serde_json::json!({"pattern": "fn main"});
        assert_eq!(c.classify("custom_tool", Some(&search_input)), ToolCategory::Search);
    }

    #[test]
    fn test_config_overrides() {
        let mappings = ToolMappings {
            file_read: vec!["my_reader".to_string()],
            file_write: vec![],
            shell: vec![],
            search: vec![],
        };
        let c = ToolClassifier::new(Some(&mappings));
        assert_eq!(c.classify("my_reader", None), ToolCategory::FileRead);
    }

    #[test]
    fn test_unknown_tool() {
        let c = classifier();
        assert!(matches!(c.classify("totally_custom", None), ToolCategory::Other(_)));
    }

    #[test]
    fn test_extract_helpers() {
        let c = classifier();

        let input = serde_json::json!({"file_path": "/src/main.rs"});
        assert_eq!(c.extract_file_path(&input), Some("/src/main.rs"));

        let input2 = serde_json::json!({"path": "/src/lib.rs"});
        assert_eq!(c.extract_file_path(&input2), Some("/src/lib.rs"));

        let input3 = serde_json::json!({"command": "cargo build"});
        assert_eq!(c.extract_command(&input3), Some("cargo build"));

        let input4 = serde_json::json!({"pattern": "fn main"});
        assert_eq!(c.extract_pattern(&input4), Some("fn main"));
    }

    #[test]
    fn test_observation_categories() {
        assert_eq!(ToolCategory::FileRead.observation_category(), "exploration");
        assert_eq!(ToolCategory::Search.observation_category(), "exploration");
        assert_eq!(ToolCategory::FileWrite.observation_category(), "modification");
        assert_eq!(ToolCategory::Shell.observation_category(), "command");
        assert_eq!(ToolCategory::Other("x".into()).observation_category(), "tool");
    }
}
