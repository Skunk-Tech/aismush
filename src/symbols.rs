//! AST-based symbol extraction using Tree-sitter.
//!
//! Replaces the regex-based import parsing in deps.rs with proper AST traversal,
//! giving us symbol-level dependency analysis: function calls, type references,
//! class inheritance — the full graph that GitNexus provides, natively in Rust.
//!
//! Supported languages: Rust, TypeScript, JavaScript, Python, Go, Java.
//! Falls back to empty results for unsupported languages (file-level deps still work).

use std::collections::HashMap;
use rusqlite::params;
use tracing::{debug, warn};

use crate::db::Db;

// ── Public types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SymbolKind {
    Function,
    Method,
    Class,
    Struct,
    Trait,
    Interface,
    Type,
    Enum,
    Constant,
    Variable,
    Module,
}

impl SymbolKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            SymbolKind::Function  => "function",
            SymbolKind::Method    => "method",
            SymbolKind::Class     => "class",
            SymbolKind::Struct    => "struct",
            SymbolKind::Trait     => "trait",
            SymbolKind::Interface => "interface",
            SymbolKind::Type      => "type",
            SymbolKind::Enum      => "enum",
            SymbolKind::Constant  => "constant",
            SymbolKind::Variable  => "variable",
            SymbolKind::Module    => "module",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExtractedSymbol {
    pub name: String,
    pub kind: SymbolKind,
    pub start_line: u32,
    pub end_line: u32,
    pub is_exported: bool,
    pub signature: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefKind {
    Call,
    Import,
}

impl RefKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            RefKind::Call   => "call",
            RefKind::Import => "import",
        }
    }
}

#[derive(Debug, Clone)]
pub struct SymbolRef {
    pub from_symbol: String,
    pub to_symbol: String,
    pub to_file_hint: String,
    pub kind: RefKind,
}

#[derive(Debug, Default)]
pub struct FileSymbols {
    pub symbols: Vec<ExtractedSymbol>,
    pub refs: Vec<SymbolRef>,
}

/// Result type for symbol search queries.
#[derive(Debug, serde::Serialize)]
pub struct SymbolSearchHit {
    pub symbol_name: String,
    pub symbol_kind: String,
    pub file_path: String,
    pub signature: String,
    pub blast_radius_score: f64,
}

// ── Top-level entry point ────────────────────────────────────────────────────

/// Extract all symbols and references from source content.
/// Dispatches to a language-specific Tree-sitter parser.
pub fn extract_symbols(content: &str, language: &str) -> FileSymbols {
    match language {
        "rust"                     => extract_rust(content),
        "typescript" | "tsx"       => extract_typescript(content),
        "javascript" | "jsx"       => extract_javascript(content),
        "python"                   => extract_python(content),
        "go"                       => extract_go(content),
        "java"                     => extract_java(content),
        _                          => FileSymbols::default(),
    }
}

// ── Rust extraction ──────────────────────────────────────────────────────────

fn extract_rust(content: &str) -> FileSymbols {
    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&tree_sitter_rust::language()).is_err() {
        warn!("Failed to load Rust grammar");
        return FileSymbols::default();
    }

    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return FileSymbols::default(),
    };

    let mut symbols = Vec::new();
    let mut refs = Vec::new();
    let bytes = content.as_bytes();

    walk_node(tree.root_node(), &mut |node| {
        match node.kind() {
            "function_item" | "function_signature_item" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = node_text(name_node, bytes);
                    let is_pub = is_rust_pub(node, bytes);
                    let sig = first_line_of_node(node, content);
                    symbols.push(ExtractedSymbol {
                        name,
                        kind: SymbolKind::Function,
                        start_line: node.start_position().row as u32 + 1,
                        end_line: node.end_position().row as u32 + 1,
                        is_exported: is_pub,
                        signature: sig,
                    });
                }
            }
            "struct_item" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = node_text(name_node, bytes);
                    let is_pub = is_rust_pub(node, bytes);
                    symbols.push(ExtractedSymbol {
                        name,
                        kind: SymbolKind::Struct,
                        start_line: node.start_position().row as u32 + 1,
                        end_line: node.end_position().row as u32 + 1,
                        is_exported: is_pub,
                        signature: first_line_of_node(node, content),
                    });
                }
            }
            "enum_item" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = node_text(name_node, bytes);
                    let is_pub = is_rust_pub(node, bytes);
                    symbols.push(ExtractedSymbol {
                        name,
                        kind: SymbolKind::Enum,
                        start_line: node.start_position().row as u32 + 1,
                        end_line: node.end_position().row as u32 + 1,
                        is_exported: is_pub,
                        signature: first_line_of_node(node, content),
                    });
                }
            }
            "trait_item" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = node_text(name_node, bytes);
                    let is_pub = is_rust_pub(node, bytes);
                    symbols.push(ExtractedSymbol {
                        name,
                        kind: SymbolKind::Trait,
                        start_line: node.start_position().row as u32 + 1,
                        end_line: node.end_position().row as u32 + 1,
                        is_exported: is_pub,
                        signature: first_line_of_node(node, content),
                    });
                }
            }
            "type_item" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = node_text(name_node, bytes);
                    let is_pub = is_rust_pub(node, bytes);
                    symbols.push(ExtractedSymbol {
                        name,
                        kind: SymbolKind::Type,
                        start_line: node.start_position().row as u32 + 1,
                        end_line: node.end_position().row as u32 + 1,
                        is_exported: is_pub,
                        signature: first_line_of_node(node, content),
                    });
                }
            }
            "const_item" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = node_text(name_node, bytes);
                    let is_pub = is_rust_pub(node, bytes);
                    symbols.push(ExtractedSymbol {
                        name,
                        kind: SymbolKind::Constant,
                        start_line: node.start_position().row as u32 + 1,
                        end_line: node.end_position().row as u32 + 1,
                        is_exported: is_pub,
                        signature: first_line_of_node(node, content),
                    });
                }
            }
            "call_expression" => {
                // function() or object.method() calls
                if let Some(func_node) = node.child_by_field_name("function") {
                    let callee = node_text(func_node, bytes);
                    // Strip path prefix: foo::bar::baz → baz
                    let short = callee.split("::").last().unwrap_or(&callee).to_string();
                    if !short.is_empty() && !short.starts_with(|c: char| c.is_uppercase()) {
                        refs.push(SymbolRef {
                            from_symbol: String::new(),
                            to_symbol: short,
                            to_file_hint: String::new(),
                            kind: RefKind::Call,
                        });
                    }
                }
            }
            "use_declaration" => {
                let text = node_text(node, bytes);
                // use crate::foo::Bar → Bar is a type import
                if let Some(last_seg) = text.trim_end_matches(';')
                    .split("::")
                    .last()
                    .map(|s| s.trim_matches(|c| c == '{' || c == '}' || c == ' '))
                {
                    for part in last_seg.split(',') {
                        let sym = part.trim().trim_matches(|c: char| !c.is_alphanumeric() && c != '_');
                        if !sym.is_empty() {
                            refs.push(SymbolRef {
                                from_symbol: String::new(),
                                to_symbol: sym.to_string(),
                                to_file_hint: String::new(),
                                kind: RefKind::Import,
                            });
                        }
                    }
                }
            }
            _ => {}
        }
    });

    FileSymbols { symbols, refs }
}

fn is_rust_pub(node: tree_sitter::Node, bytes: &[u8]) -> bool {
    // Check if any sibling or parent contains "pub"
    let start = node.start_byte().saturating_sub(8);
    let end = node.start_byte();
    if let Ok(prefix) = std::str::from_utf8(&bytes[start..end]) {
        return prefix.contains("pub");
    }
    false
}

// ── TypeScript extraction ────────────────────────────────────────────────────

fn extract_typescript(content: &str) -> FileSymbols {
    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&tree_sitter_typescript::language_typescript()).is_err() {
        warn!("Failed to load TypeScript grammar");
        return FileSymbols::default();
    }
    extract_ts_js_common(&mut parser, content, true)
}

// ── JavaScript extraction ────────────────────────────────────────────────────

fn extract_javascript(content: &str) -> FileSymbols {
    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&tree_sitter_javascript::language()).is_err() {
        warn!("Failed to load JavaScript grammar");
        return FileSymbols::default();
    }
    extract_ts_js_common(&mut parser, content, false)
}

fn extract_ts_js_common(parser: &mut tree_sitter::Parser, content: &str, is_ts: bool) -> FileSymbols {
    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return FileSymbols::default(),
    };

    let mut symbols = Vec::new();
    let mut refs = Vec::new();
    let bytes = content.as_bytes();

    walk_node(tree.root_node(), &mut |node| {
        match node.kind() {
            "function_declaration" | "function" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = node_text(name_node, bytes);
                    symbols.push(ExtractedSymbol {
                        name,
                        kind: SymbolKind::Function,
                        start_line: node.start_position().row as u32 + 1,
                        end_line: node.end_position().row as u32 + 1,
                        is_exported: is_js_exported(node),
                        signature: first_line_of_node(node, content),
                    });
                }
            }
            "class_declaration" | "class" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = node_text(name_node, bytes);
                    symbols.push(ExtractedSymbol {
                        name,
                        kind: SymbolKind::Class,
                        start_line: node.start_position().row as u32 + 1,
                        end_line: node.end_position().row as u32 + 1,
                        is_exported: is_js_exported(node),
                        signature: first_line_of_node(node, content),
                    });
                }
            }
            "method_definition" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = node_text(name_node, bytes);
                    if name != "constructor" {
                        symbols.push(ExtractedSymbol {
                            name,
                            kind: SymbolKind::Method,
                            start_line: node.start_position().row as u32 + 1,
                            end_line: node.end_position().row as u32 + 1,
                            is_exported: false,
                            signature: first_line_of_node(node, content),
                        });
                    }
                }
            }
            "type_alias_declaration" if is_ts => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = node_text(name_node, bytes);
                    symbols.push(ExtractedSymbol {
                        name,
                        kind: SymbolKind::Type,
                        start_line: node.start_position().row as u32 + 1,
                        end_line: node.end_position().row as u32 + 1,
                        is_exported: is_js_exported(node),
                        signature: first_line_of_node(node, content),
                    });
                }
            }
            "interface_declaration" if is_ts => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = node_text(name_node, bytes);
                    symbols.push(ExtractedSymbol {
                        name,
                        kind: SymbolKind::Interface,
                        start_line: node.start_position().row as u32 + 1,
                        end_line: node.end_position().row as u32 + 1,
                        is_exported: is_js_exported(node),
                        signature: first_line_of_node(node, content),
                    });
                }
            }
            "call_expression" => {
                if let Some(func_node) = node.child_by_field_name("function") {
                    let callee = node_text(func_node, bytes);
                    // Strip property access: foo.bar.baz → baz
                    let short = callee.split('.').last().unwrap_or(&callee).to_string();
                    if !short.is_empty() {
                        refs.push(SymbolRef {
                            from_symbol: String::new(),
                            to_symbol: short,
                            to_file_hint: String::new(),
                            kind: RefKind::Call,
                        });
                    }
                }
            }
            "import_statement" => {
                let text = node_text(node, bytes);
                // import { Foo, Bar } from './module'
                // Capture source path for file-level ref
                if let Some(src) = extract_import_source(&text) {
                    refs.push(SymbolRef {
                        from_symbol: String::new(),
                        to_symbol: String::new(),
                        to_file_hint: src,
                        kind: RefKind::Import,
                    });
                }
            }
            _ => {}
        }
    });

    FileSymbols { symbols, refs }
}

fn is_js_exported(node: tree_sitter::Node) -> bool {
    if let Some(parent) = node.parent() {
        return parent.kind() == "export_statement";
    }
    false
}

fn extract_import_source(import_text: &str) -> Option<String> {
    // import ... from 'path' or import ... from "path"
    let after_from = import_text.split(" from ").last()?;
    let path = after_from.trim().trim_matches(|c| c == '\'' || c == '"' || c == ';');
    if path.starts_with('.') {
        Some(path.to_string())
    } else {
        None
    }
}

// ── Python extraction ────────────────────────────────────────────────────────

fn extract_python(content: &str) -> FileSymbols {
    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&tree_sitter_python::language()).is_err() {
        warn!("Failed to load Python grammar");
        return FileSymbols::default();
    }

    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return FileSymbols::default(),
    };

    let mut symbols = Vec::new();
    let mut refs = Vec::new();
    let bytes = content.as_bytes();

    walk_node(tree.root_node(), &mut |node| {
        match node.kind() {
            "function_definition" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = node_text(name_node, bytes);
                    let is_pub = !name.starts_with('_');
                    symbols.push(ExtractedSymbol {
                        name,
                        kind: SymbolKind::Function,
                        start_line: node.start_position().row as u32 + 1,
                        end_line: node.end_position().row as u32 + 1,
                        is_exported: is_pub,
                        signature: first_line_of_node(node, content),
                    });
                }
            }
            "class_definition" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = node_text(name_node, bytes);
                    let is_pub = !name.starts_with('_');
                    symbols.push(ExtractedSymbol {
                        name,
                        kind: SymbolKind::Class,
                        start_line: node.start_position().row as u32 + 1,
                        end_line: node.end_position().row as u32 + 1,
                        is_exported: is_pub,
                        signature: first_line_of_node(node, content),
                    });
                }
            }
            "call" => {
                if let Some(func_node) = node.child_by_field_name("function") {
                    let callee = node_text(func_node, bytes);
                    let short = callee.split('.').last().unwrap_or(&callee).to_string();
                    if !short.is_empty() && short.chars().all(|c| c.is_alphanumeric() || c == '_') {
                        refs.push(SymbolRef {
                            from_symbol: String::new(),
                            to_symbol: short,
                            to_file_hint: String::new(),
                            kind: RefKind::Call,
                        });
                    }
                }
            }
            "import_from_statement" | "import_statement" => {
                let text = node_text(node, bytes);
                // from .module import Foo → relative import
                if text.starts_with("from .") || text.starts_with("from ..") {
                    let parts: Vec<&str> = text.splitn(3, ' ').collect();
                    if parts.len() >= 2 {
                        refs.push(SymbolRef {
                            from_symbol: String::new(),
                            to_symbol: String::new(),
                            to_file_hint: parts[1].replace('.', "/"),
                            kind: RefKind::Import,
                        });
                    }
                }
            }
            _ => {}
        }
    });

    FileSymbols { symbols, refs }
}

// ── Go extraction ────────────────────────────────────────────────────────────

fn extract_go(content: &str) -> FileSymbols {
    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&tree_sitter_go::language()).is_err() {
        warn!("Failed to load Go grammar");
        return FileSymbols::default();
    }

    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return FileSymbols::default(),
    };

    let mut symbols = Vec::new();
    let mut refs = Vec::new();
    let bytes = content.as_bytes();

    walk_node(tree.root_node(), &mut |node| {
        match node.kind() {
            "function_declaration" | "method_declaration" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = node_text(name_node, bytes);
                    // Go exports are uppercase
                    let is_pub = name.chars().next().map(|c| c.is_uppercase()).unwrap_or(false);
                    let kind = if node.kind() == "method_declaration" {
                        SymbolKind::Method
                    } else {
                        SymbolKind::Function
                    };
                    symbols.push(ExtractedSymbol {
                        name,
                        kind,
                        start_line: node.start_position().row as u32 + 1,
                        end_line: node.end_position().row as u32 + 1,
                        is_exported: is_pub,
                        signature: first_line_of_node(node, content),
                    });
                }
            }
            "type_declaration" => {
                // type Foo struct{} or type Bar interface{}
                for i in 0..node.child_count() {
                    if let Some(spec) = node.child(i) {
                        if spec.kind() == "type_spec" {
                            if let Some(name_node) = spec.child_by_field_name("name") {
                                let name = node_text(name_node, bytes);
                                let is_pub = name.chars().next().map(|c| c.is_uppercase()).unwrap_or(false);
                                let kind = if let Some(type_node) = spec.child_by_field_name("type") {
                                    match type_node.kind() {
                                        "struct_type"    => SymbolKind::Struct,
                                        "interface_type" => SymbolKind::Interface,
                                        _                => SymbolKind::Type,
                                    }
                                } else {
                                    SymbolKind::Type
                                };
                                symbols.push(ExtractedSymbol {
                                    name,
                                    kind,
                                    start_line: node.start_position().row as u32 + 1,
                                    end_line: node.end_position().row as u32 + 1,
                                    is_exported: is_pub,
                                    signature: first_line_of_node(node, content),
                                });
                            }
                        }
                    }
                }
            }
            "call_expression" => {
                if let Some(func_node) = node.child_by_field_name("function") {
                    let callee = node_text(func_node, bytes);
                    let short = callee.split('.').last().unwrap_or(&callee).to_string();
                    if !short.is_empty() {
                        refs.push(SymbolRef {
                            from_symbol: String::new(),
                            to_symbol: short,
                            to_file_hint: String::new(),
                            kind: RefKind::Call,
                        });
                    }
                }
            }
            _ => {}
        }
    });

    FileSymbols { symbols, refs }
}

// ── Java extraction ──────────────────────────────────────────────────────────

fn extract_java(content: &str) -> FileSymbols {
    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&tree_sitter_java::language()).is_err() {
        warn!("Failed to load Java grammar");
        return FileSymbols::default();
    }

    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => return FileSymbols::default(),
    };

    let mut symbols = Vec::new();
    let mut refs = Vec::new();
    let bytes = content.as_bytes();

    walk_node(tree.root_node(), &mut |node| {
        match node.kind() {
            "method_declaration" | "constructor_declaration" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = node_text(name_node, bytes);
                    let is_pub = is_java_public(node, bytes);
                    symbols.push(ExtractedSymbol {
                        name,
                        kind: SymbolKind::Method,
                        start_line: node.start_position().row as u32 + 1,
                        end_line: node.end_position().row as u32 + 1,
                        is_exported: is_pub,
                        signature: first_line_of_node(node, content),
                    });
                }
            }
            "class_declaration" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = node_text(name_node, bytes);
                    let is_pub = is_java_public(node, bytes);
                    symbols.push(ExtractedSymbol {
                        name,
                        kind: SymbolKind::Class,
                        start_line: node.start_position().row as u32 + 1,
                        end_line: node.end_position().row as u32 + 1,
                        is_exported: is_pub,
                        signature: first_line_of_node(node, content),
                    });
                }
            }
            "interface_declaration" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = node_text(name_node, bytes);
                    let is_pub = is_java_public(node, bytes);
                    symbols.push(ExtractedSymbol {
                        name,
                        kind: SymbolKind::Interface,
                        start_line: node.start_position().row as u32 + 1,
                        end_line: node.end_position().row as u32 + 1,
                        is_exported: is_pub,
                        signature: first_line_of_node(node, content),
                    });
                }
            }
            "method_invocation" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = node_text(name_node, bytes);
                    if !name.is_empty() {
                        refs.push(SymbolRef {
                            from_symbol: String::new(),
                            to_symbol: name,
                            to_file_hint: String::new(),
                            kind: RefKind::Call,
                        });
                    }
                }
            }
            _ => {}
        }
    });

    FileSymbols { symbols, refs }
}

fn is_java_public(node: tree_sitter::Node, bytes: &[u8]) -> bool {
    let start = node.start_byte().saturating_sub(20);
    let end = node.start_byte();
    if let Ok(prefix) = std::str::from_utf8(&bytes[start..end]) {
        return prefix.contains("public");
    }
    false
}

// ── Database operations ──────────────────────────────────────────────────────

/// Store extracted symbols and refs for a single file.
/// Replaces any previous data for this (project, file) pair.
pub async fn store_symbols(
    db: &Db,
    project_path: &str,
    file_path: &str,
    file_symbols: &FileSymbols,
) {
    let db = db.clone();
    let project = project_path.to_string();
    let file = file_path.to_string();
    let symbols: Vec<(String, String, u32, u32, bool, String)> = file_symbols.symbols.iter()
        .map(|s| (s.name.clone(), s.kind.as_str().to_string(), s.start_line, s.end_line, s.is_exported, s.signature.clone()))
        .collect();
    let refs_data: Vec<(String, String, String, String)> = file_symbols.refs.iter()
        .map(|r| (r.from_symbol.clone(), r.to_file_hint.clone(), r.to_symbol.clone(), r.kind.as_str().to_string()))
        .collect();

    tokio::task::spawn_blocking(move || {
        let conn = db.blocking_lock();

        // Clear old data for this file
        conn.execute(
            "DELETE FROM code_symbols WHERE project_path = ?1 AND file_path = ?2",
            params![project, file],
        ).ok();
        conn.execute(
            "DELETE FROM symbol_refs WHERE project_path = ?1 AND from_file = ?2",
            params![project, file],
        ).ok();

        // Insert symbols
        for (name, kind, start_line, end_line, is_exported, sig) in &symbols {
            conn.execute(
                "INSERT OR IGNORE INTO code_symbols (project_path, file_path, symbol_name, symbol_kind, start_line, end_line, is_exported, signature)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![project, file, name, kind, *start_line as i64, *end_line as i64, *is_exported as i64, sig],
            ).ok();
        }

        // Insert refs
        for (from_sym, to_file, to_sym, ref_kind) in &refs_data {
            if to_sym.is_empty() && to_file.is_empty() { continue; }
            conn.execute(
                "INSERT OR IGNORE INTO symbol_refs (project_path, from_file, from_symbol, to_file, to_symbol, ref_kind)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![project, file, from_sym, to_file, to_sym, ref_kind],
            ).ok();
        }

        debug!(project = %project, file = %file, symbols = symbols.len(), refs = refs_data.len(), "Symbols stored");
    }).await.ok();
}

/// Delete all symbols for a list of deleted files.
pub async fn delete_symbols(db: &Db, project_path: &str, file_paths: &[String]) {
    if file_paths.is_empty() { return; }
    let db = db.clone();
    let project = project_path.to_string();
    let files = file_paths.to_vec();

    tokio::task::spawn_blocking(move || {
        let conn = db.blocking_lock();
        for file in &files {
            conn.execute(
                "DELETE FROM code_symbols WHERE project_path = ?1 AND file_path = ?2",
                params![project, file],
            ).ok();
            conn.execute(
                "DELETE FROM symbol_refs WHERE project_path = ?1 AND from_file = ?2",
                params![project, file],
            ).ok();
            conn.execute(
                "DELETE FROM symbol_blast_radius WHERE project_path = ?1 AND file_path = ?2",
                params![project, file],
            ).ok();
        }
    }).await.ok();
}

/// Get all exported symbols for a file.
pub async fn get_file_exports(db: &Db, project_path: &str, file_path: &str) -> Vec<ExtractedSymbol> {
    let db = db.clone();
    let project = project_path.to_string();
    let file = file_path.to_string();

    tokio::task::spawn_blocking(move || {
        let conn = db.blocking_lock();
        let mut stmt = conn.prepare(
            "SELECT symbol_name, symbol_kind, start_line, end_line, is_exported, signature
             FROM code_symbols
             WHERE project_path = ?1 AND file_path = ?2
             ORDER BY start_line"
        ).ok()?;

        let rows = stmt.query_map(params![project, file], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, String>(5)?,
            ))
        }).ok()?;

        Some(rows.filter_map(|r| r.ok()).map(|(name, kind_str, start, end, exported, sig)| {
            ExtractedSymbol {
                name,
                kind: parse_kind(&kind_str),
                start_line: start as u32,
                end_line: end as u32,
                is_exported: exported != 0,
                signature: sig,
            }
        }).collect())
    }).await.ok().flatten().unwrap_or_default()
}

/// BM25 keyword search over symbol names using FTS5.
pub async fn search_symbols(
    db: &Db,
    project_path: &str,
    query: &str,
    limit: usize,
) -> Vec<(String, String, String, String, f64)> {
    let db = db.clone();
    let project = project_path.to_string();
    let q = query.to_string();
    let limit = limit as i64;

    tokio::task::spawn_blocking(move || {
        let conn = db.blocking_lock();
        let mut stmt = conn.prepare(
            "SELECT cs.symbol_name, cs.symbol_kind, cs.file_path, cs.signature,
                    COALESCE(sbr.score, 0.0) as blast_score
             FROM symbols_fts sf
             JOIN code_symbols cs ON sf.rowid = cs.id
             LEFT JOIN symbol_blast_radius sbr
               ON sbr.project_path = cs.project_path
               AND sbr.file_path = cs.file_path
               AND sbr.symbol_name = cs.symbol_name
             WHERE symbols_fts MATCH ?1 AND cs.project_path = ?2
             ORDER BY blast_score DESC, rank
             LIMIT ?3"
        ).ok()?;

        let rows = stmt.query_map(params![q, project, limit], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, f64>(4)?,
            ))
        }).ok()?;

        Some(rows.filter_map(|r| r.ok()).collect())
    }).await.ok().flatten().unwrap_or_default()
}

/// Compute and store symbol-level blast radius for all symbols in a project.
/// Called after symbol extraction during --scan.
pub async fn compute_symbol_blast_radii(db: &Db, project_path: &str) {
    let db = db.clone();
    let project = project_path.to_string();

    tokio::task::spawn_blocking(move || {
        let conn = db.blocking_lock();

        // Get all symbols with their files
        let symbols: Vec<(String, String)> = {
            conn.prepare(
                "SELECT DISTINCT file_path, symbol_name FROM code_symbols WHERE project_path = ?1"
            ).ok().and_then(|mut s| {
                let rows = s.query_map(params![project], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                }).ok()?;
                Some(rows.filter_map(|r| r.ok()).collect())
            }).unwrap_or_default()
        };

        // Build in-memory ref graph: to_symbol → vec of (from_file, from_symbol)
        let ref_graph: HashMap<String, Vec<(String, String)>> = {
            let mut map: HashMap<String, Vec<(String, String)>> = HashMap::new();
            let mut stmt = conn.prepare(
                "SELECT from_file, from_symbol, to_symbol FROM symbol_refs WHERE project_path = ?1"
            ).ok().unwrap();
            if let Ok(rows) = stmt.query_map(params![project], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?))
            }) {
                for row in rows.flatten() {
                    map.entry(row.2).or_default().push((row.0, row.1));
                }
            }
            map
        };

        // For each symbol, BFS through callers to count transitive reach
        for (file_path, symbol_name) in &symbols {
            let direct = ref_graph.get(symbol_name.as_str()).map(|v| v.len()).unwrap_or(0);

            // BFS
            let mut visited = std::collections::HashSet::new();
            let mut queue = std::collections::VecDeque::new();
            visited.insert(symbol_name.clone());
            queue.push_back(symbol_name.clone());

            while let Some(current) = queue.pop_front() {
                if let Some(callers) = ref_graph.get(&current) {
                    for (_, caller_sym) in callers {
                        if !caller_sym.is_empty() && visited.insert(caller_sym.clone()) {
                            queue.push_back(caller_sym.clone());
                        }
                    }
                }
            }
            let transitive = visited.len().saturating_sub(1);

            let score = if transitive == 0 {
                0.0_f64
            } else {
                ((1.0_f64 + transitive as f64).ln() / (50.0_f64).ln()).min(1.0_f64)
            };

            conn.execute(
                "INSERT OR REPLACE INTO symbol_blast_radius (project_path, file_path, symbol_name, direct_callers, transitive_callers, score)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![project, file_path, symbol_name, direct as i64, transitive as i64, score],
            ).ok();
        }
    }).await.ok();
}

// ── Tree-sitter helpers ──────────────────────────────────────────────────────

/// Recursively walk all nodes in a tree, calling f on each.
fn walk_node<F: FnMut(tree_sitter::Node)>(node: tree_sitter::Node, f: &mut F) {
    f(node);
    let mut cursor = node.walk();
    if cursor.goto_first_child() {
        loop {
            walk_node(cursor.node(), f);
            if !cursor.goto_next_sibling() { break; }
        }
    }
}

/// Extract the text of a node from the source bytes.
fn node_text(node: tree_sitter::Node, bytes: &[u8]) -> String {
    std::str::from_utf8(&bytes[node.start_byte()..node.end_byte()])
        .unwrap_or("")
        .to_string()
}

/// Extract the first line of a node as its "signature".
fn first_line_of_node(node: tree_sitter::Node, content: &str) -> String {
    let start = node.start_byte();
    // Clamp end to a valid char boundary so slicing never panics on multibyte chars
    let raw_end = content.len().min(start + 200);
    let mut end = raw_end;
    while end > start && !content.is_char_boundary(end) {
        end -= 1;
    }
    content[start..end]
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string()
}

fn parse_kind(s: &str) -> SymbolKind {
    match s {
        "function"  => SymbolKind::Function,
        "method"    => SymbolKind::Method,
        "class"     => SymbolKind::Class,
        "struct"    => SymbolKind::Struct,
        "trait"     => SymbolKind::Trait,
        "interface" => SymbolKind::Interface,
        "type"      => SymbolKind::Type,
        "enum"      => SymbolKind::Enum,
        "constant"  => SymbolKind::Constant,
        "variable"  => SymbolKind::Variable,
        _           => SymbolKind::Module,
    }
}
