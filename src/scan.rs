//! Project scanner and agent generator.
//!
//! Scans a codebase, analyzes it through the proxy (DeepSeek = cheap),
//! and generates optimized `.claude/agents/` and `.claude/skills/` files.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::io::Write;

/// Detected project profile.
#[derive(Debug)]
pub struct ProjectProfile {
    pub root: PathBuf,
    pub languages: Vec<String>,
    pub frameworks: Vec<String>,
    pub key_files: Vec<String>,
    pub total_files: usize,
    pub file_samples: HashMap<String, Vec<String>>, // language -> sample file paths
}

/// Scan a project directory and build a profile.
pub fn scan_project(path: &Path) -> ProjectProfile {
    let mut languages: HashMap<String, usize> = HashMap::new();
    let mut frameworks: HashSet<String> = HashSet::new();
    let mut key_files: Vec<String> = Vec::new();
    let mut file_samples: HashMap<String, Vec<String>> = HashMap::new();
    let mut total_files = 0;

    let ignore = &[
        "node_modules", ".git", "target", "dist", "build", "__pycache__",
        ".next", ".nuxt", "vendor", "venv", ".venv", "env",
        ".claude", ".roo", "coverage",
    ];

    walk_dir(path, ignore, &mut |file_path| {
        total_files += 1;
        let rel = file_path.strip_prefix(path).unwrap_or(file_path);
        let rel_str = rel.to_string_lossy().to_string();
        let name = file_path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let ext = file_path.extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_default();

        // Count languages by extension
        let lang = match ext.as_str() {
            "rs" => "Rust",
            "py" | "pyw" => "Python",
            "js" | "mjs" | "cjs" => "JavaScript",
            "ts" | "tsx" => "TypeScript",
            "go" => "Go",
            "java" | "kt" => "Java/Kotlin",
            "rb" => "Ruby",
            "php" => "PHP",
            "cs" => "C#",
            "c" | "h" => "C",
            "cpp" | "cc" | "hpp" => "C++",
            "swift" => "Swift",
            "dart" => "Dart",
            "ex" | "exs" => "Elixir",
            _ => "",
        };
        if !lang.is_empty() {
            *languages.entry(lang.to_string()).or_default() += 1;
            let samples = file_samples.entry(lang.to_string()).or_default();
            if samples.len() < 5 {
                samples.push(rel_str.clone());
            }
        }

        // Detect frameworks from key files
        match name.as_str() {
            "Cargo.toml" => { frameworks.insert("Cargo/Rust workspace".into()); key_files.push(rel_str.clone()); }
            "package.json" => { frameworks.insert("Node.js/npm".into()); key_files.push(rel_str.clone()); }
            "pyproject.toml" | "setup.py" | "requirements.txt" => { frameworks.insert("Python project".into()); key_files.push(rel_str.clone()); }
            "go.mod" => { frameworks.insert("Go module".into()); key_files.push(rel_str.clone()); }
            "Gemfile" => { frameworks.insert("Ruby/Bundler".into()); key_files.push(rel_str.clone()); }
            "composer.json" => { frameworks.insert("PHP/Composer".into()); key_files.push(rel_str.clone()); }
            "Dockerfile" | "docker-compose.yml" | "docker-compose.yaml" => { frameworks.insert("Docker".into()); }
            "vite.config.ts" | "vite.config.js" => { frameworks.insert("Vite".into()); }
            "next.config.js" | "next.config.ts" | "next.config.mjs" => { frameworks.insert("Next.js".into()); }
            "tailwind.config.js" | "tailwind.config.ts" => { frameworks.insert("Tailwind CSS".into()); }
            "tsconfig.json" => { frameworks.insert("TypeScript".into()); key_files.push(rel_str.clone()); }
            "jest.config.js" | "jest.config.ts" | "jest.config.cjs" => { frameworks.insert("Jest".into()); }
            "vitest.config.ts" | "vitest.config.js" => { frameworks.insert("Vitest".into()); }
            ".eslintrc.json" | ".eslintrc.js" | "eslint.config.js" => { frameworks.insert("ESLint".into()); }
            "prisma" => { frameworks.insert("Prisma".into()); }
            "CLAUDE.md" => { key_files.push(rel_str.clone()); }
            "README.md" => { key_files.push(rel_str.clone()); }
            _ => {}
        }

        // Detect frameworks from directory names
        if rel_str.contains("/src/") || rel_str.starts_with("src/") {
            // Check for specific patterns in source files
        }
    });

    // Sort languages by file count
    let mut lang_vec: Vec<(String, usize)> = languages.into_iter().collect();
    lang_vec.sort_by(|a, b| b.1.cmp(&a.1));

    ProjectProfile {
        root: path.to_path_buf(),
        languages: lang_vec.iter().map(|(l, _)| l.clone()).collect(),
        frameworks: frameworks.into_iter().collect(),
        key_files,
        total_files,
        file_samples,
    }
}

fn walk_dir(dir: &Path, ignore: &[&str], callback: &mut dyn FnMut(&Path)) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();

        if ignore.iter().any(|i| *i == name) {
            continue;
        }

        if path.is_dir() {
            walk_dir(&path, ignore, callback);
        } else if path.is_file() {
            callback(&path);
        }
    }
}

/// Generate agent and skill files based on the project profile.
pub fn generate_agents(profile: &ProjectProfile) -> Vec<(PathBuf, String)> {
    let mut files: Vec<(PathBuf, String)> = Vec::new();
    let claude_dir = profile.root.join(".claude").join("agents");

    // Always generate a code-reviewer agent
    files.push((
        claude_dir.join("code-reviewer.md"),
        generate_reviewer_agent(profile),
    ));

    // Always generate a debugger agent
    files.push((
        claude_dir.join("debugger.md"),
        generate_debugger_agent(profile),
    ));

    // Language-specific agents
    for lang in &profile.languages {
        match lang.as_str() {
            "Rust" => {
                files.push((claude_dir.join("rust-expert.md"), generate_rust_agent(profile)));
            }
            "TypeScript" | "JavaScript" => {
                files.push((claude_dir.join("frontend-engineer.md"), generate_frontend_agent(profile)));
            }
            "Python" => {
                files.push((claude_dir.join("python-expert.md"), generate_python_agent(profile)));
            }
            "Go" => {
                files.push((claude_dir.join("go-expert.md"), generate_go_agent(profile)));
            }
            _ => {}
        }
    }

    // Testing agent if test frameworks detected
    if profile.frameworks.iter().any(|f| f.contains("Jest") || f.contains("Vitest") || f.contains("pytest")) {
        files.push((claude_dir.join("test-runner.md"), generate_test_agent(profile)));
    }

    // Explorer agent (always, uses Haiku for cost savings)
    files.push((
        claude_dir.join("explorer.md"),
        generate_explorer_agent(profile),
    ));

    files
}

/// Write generated files to disk.
pub fn write_agents(files: &[(PathBuf, String)]) -> usize {
    let mut written = 0;
    for (path, content) in files {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).ok();
        }
        if let Ok(mut f) = fs::File::create(path) {
            if f.write_all(content.as_bytes()).is_ok() {
                written += 1;
            }
        }
    }
    written
}

// ── Agent generators ────────────────────────────────────────────────────────

fn generate_reviewer_agent(profile: &ProjectProfile) -> String {
    let langs = profile.languages.join(", ");
    format!(r#"---
name: code-reviewer
description: Expert code reviewer for this {langs} project. Use proactively after code changes.
tools: Read, Grep, Glob, Bash
model: haiku
---

You are a senior code reviewer for a project using {langs}.

Key files in this project:
{key_files}

When invoked:
1. Run git diff to see recent changes
2. Focus on modified files
3. Review immediately

Check for:
- Code clarity and readability
- Proper error handling
- No exposed secrets
- Performance issues
- Test coverage gaps

Provide feedback by priority: Critical > Warnings > Suggestions.
Include specific fix examples.
"#,
        langs = langs,
        key_files = profile.key_files.iter().take(10).map(|f| format!("- {}", f)).collect::<Vec<_>>().join("\n"),
    )
}

fn generate_debugger_agent(profile: &ProjectProfile) -> String {
    let langs = profile.languages.join(", ");
    format!(r#"---
name: debugger
description: Debugging specialist for {langs} errors, test failures, and unexpected behavior. Use proactively when encountering issues.
tools: Read, Edit, Bash, Grep, Glob
model: sonnet
---

You are an expert debugger for a {langs} project.

When invoked:
1. Capture error message and stack trace
2. Identify reproduction steps
3. Isolate the failure
4. Implement minimal fix
5. Verify solution

Focus on root cause, not symptoms.
"#,
        langs = langs,
    )
}

fn generate_rust_agent(profile: &ProjectProfile) -> String {
    let key = profile.key_files.iter()
        .filter(|f| f.ends_with("Cargo.toml") || f.ends_with(".rs"))
        .take(5)
        .map(|f| format!("- {}", f))
        .collect::<Vec<_>>()
        .join("\n");

    format!(r#"---
name: rust-expert
description: Rust systems programming specialist. Use for Rust code, Cargo workspace, unsafe code, async/await, and performance optimization.
tools: Read, Edit, Write, Bash, Grep, Glob
model: sonnet
---

You are a Rust expert working on this project.

Key Rust files:
{key}

Conventions:
- Follow Rust 2021 edition idioms
- Use proper error handling (thiserror/anyhow)
- Run `cargo clippy` before suggesting changes
- Prefer zero-copy and borrowing over cloning
- Use `tokio` for async where applicable
"#,
        key = key,
    )
}

fn generate_frontend_agent(profile: &ProjectProfile) -> String {
    let frameworks: Vec<&String> = profile.frameworks.iter()
        .filter(|f| f.contains("Vite") || f.contains("Next") || f.contains("React") || f.contains("Tailwind") || f.contains("TypeScript"))
        .collect();
    let fw_list = if frameworks.is_empty() { "TypeScript/JavaScript".to_string() } else { frameworks.iter().map(|f| f.as_str()).collect::<Vec<_>>().join(", ") };

    format!(r#"---
name: frontend-engineer
description: Frontend specialist for {fw}. Use for React components, TypeScript, CSS, and UI work.
tools: Read, Edit, Write, Bash, Grep, Glob
model: sonnet
---

You are a frontend engineer working with {fw}.

Follow project conventions:
- Check existing component patterns before creating new ones
- Use project's import style (check tsconfig paths)
- Match existing CSS approach (modules, tailwind, or plain CSS)
- Write tests matching the project's test framework
"#,
        fw = fw_list,
    )
}

fn generate_python_agent(profile: &ProjectProfile) -> String {
    format!(r#"---
name: python-expert
description: Python specialist. Use for Python code, data processing, APIs, and scripting.
tools: Read, Edit, Write, Bash, Grep, Glob
model: sonnet
---

You are a Python expert.

Follow project conventions:
- Check for pyproject.toml/setup.py for project config
- Use type hints consistently
- Follow existing import style
- Run linting before suggesting changes (ruff/black/mypy)
"#)
}

fn generate_go_agent(_profile: &ProjectProfile) -> String {
    format!(r#"---
name: go-expert
description: Go specialist. Use for Go code, concurrency, and performance.
tools: Read, Edit, Write, Bash, Grep, Glob
model: sonnet
---

You are a Go expert.

Follow Go conventions:
- gofmt formatting
- Effective Go idioms
- Proper error handling (no panic in library code)
- Use go vet and staticcheck
"#)
}

fn generate_test_agent(profile: &ProjectProfile) -> String {
    let test_frameworks: Vec<&String> = profile.frameworks.iter()
        .filter(|f| f.contains("Jest") || f.contains("Vitest") || f.contains("pytest") || f.contains("cargo"))
        .collect();
    let fw = if test_frameworks.is_empty() { "the project's test framework".to_string() } else { test_frameworks.iter().map(|f| f.as_str()).collect::<Vec<_>>().join(", ") };

    format!(r#"---
name: test-runner
description: Testing specialist. Runs tests, diagnoses failures, and improves coverage. Use proactively after code changes.
tools: Read, Edit, Write, Bash, Grep, Glob
model: haiku
---

You are a testing specialist using {fw}.

When invoked:
1. Run the test suite
2. Report failures with context
3. Suggest fixes for failing tests
4. Identify untested code paths

Focus on meaningful tests, not coverage numbers.
"#,
        fw = fw,
    )
}

fn generate_explorer_agent(profile: &ProjectProfile) -> String {
    format!(r#"---
name: explorer
description: Fast codebase exploration agent. Use for finding files, searching code, understanding project structure. Runs on Haiku for speed and cost efficiency.
tools: Read, Grep, Glob, Bash
model: haiku
---

You are a fast codebase explorer. Your job is to find information quickly.

This project has {total} files across {langs}.

Key files:
{key_files}

Be concise. Return findings immediately. Don't over-explain.
"#,
        total = profile.total_files,
        langs = profile.languages.join(", "),
        key_files = profile.key_files.iter().take(10).map(|f| format!("- {}", f)).collect::<Vec<_>>().join("\n"),
    )
}
