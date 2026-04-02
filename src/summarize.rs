//! Structural code summarization.
//!
//! Extracts key structural elements (function signatures, type definitions,
//! imports) from source code using regex patterns. Produces compact summaries
//! that preserve the code's API surface while dropping implementation details.
//! Target: 3-5x token reduction on older tool_result messages.

/// Structural summary of a source file.
pub struct StructuralSummary {
    pub imports: Vec<String>,
    pub type_definitions: Vec<String>,
    pub function_signatures: Vec<String>,
    pub module_structure: Vec<String>,
    pub original_lines: usize,
    pub summary_lines: usize,
}

/// Generate a structural summary of source code.
pub fn summarize(content: &str, language: &str) -> StructuralSummary {
    let original_lines = content.lines().count();

    let (imports, types, functions, modules) = match language {
        "rust" => extract_rust(content),
        "typescript" | "javascript" => extract_ts_js(content),
        "python" => extract_python(content),
        "go" => extract_go(content),
        _ => extract_generic(content),
    };

    let summary_lines = imports.len() + types.len() + functions.len() + modules.len();

    StructuralSummary {
        imports,
        type_definitions: types,
        function_signatures: functions,
        module_structure: modules,
        original_lines,
        summary_lines,
    }
}

/// Format a summary as compact text for injection into tool_result.
pub fn format_summary(summary: &StructuralSummary, file_path: &str) -> String {
    let mut out = String::with_capacity(2048);

    let header = if file_path.is_empty() {
        format!("[Structural summary ({} lines → {} lines)]",
            summary.original_lines, summary.summary_lines)
    } else {
        format!("[Structural summary of {} ({} lines → {} lines)]",
            file_path, summary.original_lines, summary.summary_lines)
    };
    out.push_str(&header);
    out.push('\n');

    if !summary.imports.is_empty() {
        out.push_str("\n// Imports:\n");
        for imp in &summary.imports {
            out.push_str(imp.trim());
            out.push('\n');
        }
    }

    if !summary.type_definitions.is_empty() {
        out.push_str("\n// Types:\n");
        for td in &summary.type_definitions {
            out.push_str(td.trim());
            out.push('\n');
        }
    }

    if !summary.function_signatures.is_empty() {
        out.push_str("\n// Functions:\n");
        for fs in &summary.function_signatures {
            out.push_str(fs.trim());
            out.push('\n');
        }
    }

    if !summary.module_structure.is_empty() {
        out.push_str("\n// Modules:\n");
        for ms in &summary.module_structure {
            out.push_str(ms.trim());
            out.push('\n');
        }
    }

    // Cap at 2KB
    if out.len() > 2048 {
        out.truncate(2048);
        if let Some(last_nl) = out.rfind('\n') {
            out.truncate(last_nl);
        }
        out.push_str("\n[... summary truncated]");
    }

    out
}

/// Check if content should be summarized.
#[allow(dead_code)]
pub fn should_summarize(content_len: usize, is_recent: bool) -> bool {
    !is_recent && content_len >= 4000
}

// ── Rust extraction ─────────────────────────────────────────────────────

fn extract_rust(content: &str) -> (Vec<String>, Vec<String>, Vec<String>, Vec<String>) {
    let mut imports = Vec::new();
    let mut types = Vec::new();
    let mut functions = Vec::new();
    let mut modules = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();

        // use statements
        if trimmed.starts_with("use ") && trimmed.ends_with(';') {
            imports.push(trimmed.to_string());
            continue;
        }

        // mod declarations
        if trimmed.starts_with("mod ") && trimmed.ends_with(';') {
            modules.push(trimmed.to_string());
            continue;
        }
        if trimmed.starts_with("pub mod ") && trimmed.ends_with(';') {
            modules.push(trimmed.to_string());
            continue;
        }

        // struct/enum/trait/type definitions
        if is_rust_type_def(trimmed) {
            // Capture the line up to { or where
            let sig = extract_to_brace_or_eol(trimmed);
            types.push(format!("{} {{ ... }}", sig));
            continue;
        }

        // impl blocks
        if trimmed.starts_with("impl ") || trimmed.starts_with("impl<") {
            let sig = extract_to_brace_or_eol(trimmed);
            types.push(format!("{} {{ ... }}", sig));
            continue;
        }

        // Function signatures
        if is_rust_fn(trimmed) {
            let sig = extract_to_brace_or_eol(trimmed);
            functions.push(format!("{} {{ ... }}", sig));
        }
    }

    (imports, types, functions, modules)
}

fn is_rust_type_def(line: &str) -> bool {
    let prefixes = ["pub struct ", "pub enum ", "pub trait ", "pub type ",
                    "struct ", "enum ", "trait ", "type "];
    prefixes.iter().any(|p| line.starts_with(p))
}

fn is_rust_fn(line: &str) -> bool {
    let prefixes = ["pub fn ", "pub async fn ", "pub(crate) fn ", "pub(super) fn ",
                    "fn ", "async fn ", "pub const fn ", "pub unsafe fn ",
                    "const fn ", "unsafe fn "];
    prefixes.iter().any(|p| line.starts_with(p))
}

fn extract_to_brace_or_eol(line: &str) -> &str {
    if let Some(pos) = line.find('{') {
        line[..pos].trim()
    } else if let Some(pos) = line.find(" where") {
        &line[..pos + 6]
    } else {
        line
    }
}

// ── TypeScript/JavaScript extraction ────────────────────────────────────

fn extract_ts_js(content: &str) -> (Vec<String>, Vec<String>, Vec<String>, Vec<String>) {
    let mut imports = Vec::new();
    let mut types = Vec::new();
    let mut functions = Vec::new();
    let mut modules = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();

        // import statements
        if trimmed.starts_with("import ") {
            imports.push(trimmed.to_string());
            continue;
        }
        if trimmed.starts_with("const ") && trimmed.contains(" = require(") {
            imports.push(trimmed.to_string());
            continue;
        }

        // export from
        if trimmed.starts_with("export ") && trimmed.contains(" from ") {
            modules.push(trimmed.to_string());
            continue;
        }

        // Type definitions
        if is_ts_type_def(trimmed) {
            let sig = extract_to_brace_or_eol(trimmed);
            types.push(format!("{} {{ ... }}", sig));
            continue;
        }

        // Function/class definitions
        if is_ts_fn_or_class(trimmed) {
            let sig = extract_to_brace_or_eol(trimmed);
            functions.push(format!("{} {{ ... }}", sig));
            continue;
        }

        // Arrow function exports: export const foo = (...) =>
        if (trimmed.starts_with("export const ") || trimmed.starts_with("export let "))
            && (trimmed.contains(" = (") || trimmed.contains(" = async ("))
        {
            let sig = if let Some(pos) = trimmed.find(" =>") {
                &trimmed[..pos + 3]
            } else if let Some(pos) = trimmed.find('{') {
                trimmed[..pos].trim()
            } else {
                trimmed
            };
            functions.push(format!("{} {{ ... }}", sig));
        }
    }

    (imports, types, functions, modules)
}

fn is_ts_type_def(line: &str) -> bool {
    let prefixes = [
        "export interface ", "export type ", "export enum ",
        "interface ", "type ", "enum ",
    ];
    prefixes.iter().any(|p| line.starts_with(p))
}

fn is_ts_fn_or_class(line: &str) -> bool {
    let prefixes = [
        "export function ", "export async function ", "export default function ",
        "export class ", "export default class ", "export abstract class ",
        "function ", "async function ", "class ",
    ];
    prefixes.iter().any(|p| line.starts_with(p))
}

// ── Python extraction ───────────────────────────────────────────────────

fn extract_python(content: &str) -> (Vec<String>, Vec<String>, Vec<String>, Vec<String>) {
    let mut imports = Vec::new();
    let mut types = Vec::new();
    let mut functions = Vec::new();
    let modules = Vec::new();
    let mut prev_decorators: Vec<String> = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();

        // import / from ... import
        if trimmed.starts_with("import ") || trimmed.starts_with("from ") {
            imports.push(trimmed.to_string());
            prev_decorators.clear();
            continue;
        }

        // Decorators
        if trimmed.starts_with('@') {
            prev_decorators.push(trimmed.to_string());
            continue;
        }

        // Class definitions (top-level only — no leading whitespace)
        if (line.starts_with("class ") || line.starts_with("class\t")) && trimmed.ends_with(':') {
            let mut sig = String::new();
            for dec in &prev_decorators {
                sig.push_str(dec);
                sig.push('\n');
            }
            sig.push_str(trimmed);
            types.push(sig);
            prev_decorators.clear();
            continue;
        }

        // Function definitions (top-level)
        if (line.starts_with("def ") || line.starts_with("async def ") ||
            line.starts_with("def\t") || line.starts_with("async def\t")) && trimmed.ends_with(':')
        {
            let mut sig = String::new();
            for dec in &prev_decorators {
                sig.push_str(dec);
                sig.push('\n');
            }
            sig.push_str(trimmed);
            functions.push(sig);
            prev_decorators.clear();
            continue;
        }

        // Method definitions (one level of indent)
        if (trimmed.starts_with("def ") || trimmed.starts_with("async def ")) && trimmed.ends_with(':') {
            let indent = line.len() - line.trim_start().len();
            if indent > 0 && indent <= 8 {
                let mut sig = String::new();
                for dec in &prev_decorators {
                    sig.push_str("    ");
                    sig.push_str(dec);
                    sig.push('\n');
                }
                sig.push_str("    ");
                sig.push_str(trimmed);
                functions.push(sig);
            }
            prev_decorators.clear();
            continue;
        }

        if !trimmed.is_empty() {
            prev_decorators.clear();
        }
    }

    (imports, types, functions, modules)
}

// ── Go extraction ───────────────────────────────────────────────────────

fn extract_go(content: &str) -> (Vec<String>, Vec<String>, Vec<String>, Vec<String>) {
    let mut imports = Vec::new();
    let mut types = Vec::new();
    let mut functions = Vec::new();
    let modules = Vec::new();
    let mut in_import_block = false;

    for line in content.lines() {
        let trimmed = line.trim();

        // Import block
        if trimmed == "import (" {
            in_import_block = true;
            continue;
        }
        if in_import_block {
            if trimmed == ")" {
                in_import_block = false;
            } else if !trimmed.is_empty() {
                imports.push(format!("import {}", trimmed));
            }
            continue;
        }
        // Single import
        if trimmed.starts_with("import \"") || trimmed.starts_with("import (") {
            imports.push(trimmed.to_string());
            continue;
        }

        // Type definitions
        if trimmed.starts_with("type ") {
            let sig = extract_to_brace_or_eol(trimmed);
            types.push(format!("{} {{ ... }}", sig));
            continue;
        }

        // Function signatures
        if trimmed.starts_with("func ") {
            let sig = extract_to_brace_or_eol(trimmed);
            functions.push(format!("{} {{ ... }}", sig));
        }
    }

    (imports, types, functions, modules)
}

// ── Generic extraction (fallback) ───────────────────────────────────────

fn extract_generic(content: &str) -> (Vec<String>, Vec<String>, Vec<String>, Vec<String>) {
    let mut imports = Vec::new();
    let mut functions = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with("import ") || trimmed.starts_with("use ") ||
           trimmed.starts_with("from ") || trimmed.starts_with("#include ") ||
           trimmed.starts_with("require ") {
            imports.push(trimmed.to_string());
        }

        if trimmed.starts_with("fn ") || trimmed.starts_with("pub fn ") ||
           trimmed.starts_with("def ") || trimmed.starts_with("func ") ||
           trimmed.starts_with("function ") || trimmed.starts_with("class ") {
            let sig = extract_to_brace_or_eol(trimmed);
            functions.push(sig.to_string());
        }
    }

    (imports, Vec::new(), functions, Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rust_summary() {
        let code = r#"
use std::collections::HashMap;
use crate::db::Db;

mod config;
pub mod utils;

pub struct ProxyState {
    pub config: Config,
    pub client: HttpClient,
}

pub enum RouteDecision {
    Claude,
    DeepSeek,
}

impl ProxyState {
    pub fn new(config: Config) -> Self {
        Self { config, client: HttpClient::new() }
    }

    pub async fn handle(&self, req: Request) -> Response {
        // lots of implementation
        unimplemented!()
    }
}

fn helper() {
    println!("internal");
}
"#;
        let summary = summarize(code, "rust");
        assert_eq!(summary.imports.len(), 2);
        assert_eq!(summary.module_structure.len(), 2);
        assert!(summary.type_definitions.len() >= 3); // struct, enum, impl
        assert!(summary.function_signatures.len() >= 2); // new, handle, helper
        assert!(summary.summary_lines < summary.original_lines);
    }

    #[test]
    fn test_typescript_summary() {
        let code = r#"
import { useState } from 'react';
import type { User } from './types';

export interface AppProps {
    title: string;
    user: User;
}

export type Theme = 'light' | 'dark';

export function App({ title, user }: AppProps) {
    const [count, setCount] = useState(0);
    return <div>{title}</div>;
}

export class UserService {
    constructor(private db: Database) {}

    async getUser(id: string): Promise<User> {
        return this.db.findOne(id);
    }
}
"#;
        let summary = summarize(code, "typescript");
        assert_eq!(summary.imports.len(), 2);
        assert!(summary.type_definitions.len() >= 2); // interface, type
        assert!(summary.function_signatures.len() >= 2); // App, UserService
    }

    #[test]
    fn test_python_summary() {
        let code = r#"
import os
from pathlib import Path
from typing import Optional

@dataclass
class Config:
    host: str
    port: int

def main():
    config = load_config()
    run(config)

async def handle_request(req: Request) -> Response:
    return Response(200)

class Server:
    def __init__(self, config: Config):
        self.config = config

    async def start(self):
        pass
"#;
        let summary = summarize(code, "python");
        assert_eq!(summary.imports.len(), 3);
        assert_eq!(summary.type_definitions.len(), 2); // Config, Server
        assert!(summary.function_signatures.len() >= 2); // main, handle_request + methods
    }

    #[test]
    fn test_go_summary() {
        let code = r#"
package main

import (
    "fmt"
    "net/http"
)

type Server struct {
    Port int
    Host string
}

func NewServer(port int) *Server {
    return &Server{Port: port}
}

func (s *Server) Start() error {
    return http.ListenAndServe(fmt.Sprintf(":%d", s.Port), nil)
}
"#;
        let summary = summarize(code, "go");
        assert_eq!(summary.imports.len(), 2);
        assert_eq!(summary.type_definitions.len(), 1); // Server
        assert_eq!(summary.function_signatures.len(), 2); // NewServer, Start
    }

    #[test]
    fn test_format_summary() {
        let summary = StructuralSummary {
            imports: vec!["use std::io;".to_string()],
            type_definitions: vec!["pub struct Foo { ... }".to_string()],
            function_signatures: vec!["pub fn bar() -> Result<()> { ... }".to_string()],
            module_structure: vec!["mod utils;".to_string()],
            original_lines: 200,
            summary_lines: 4,
        };
        let formatted = format_summary(&summary, "src/main.rs");
        assert!(formatted.contains("[Structural summary of src/main.rs"));
        assert!(formatted.contains("200 lines → 4 lines"));
        assert!(formatted.contains("use std::io;"));
        assert!(formatted.contains("pub struct Foo"));
        assert!(formatted.contains("pub fn bar"));
        assert!(formatted.contains("mod utils;"));
    }

    #[test]
    fn test_should_summarize() {
        assert!(!should_summarize(3000, false)); // too small
        assert!(!should_summarize(5000, true));  // recent
        assert!(should_summarize(5000, false));  // large + not recent
    }
}
