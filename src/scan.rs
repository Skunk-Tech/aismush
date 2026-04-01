//! AI-powered project scanner.
//!
//! Scans a codebase, sends to AI through our proxy for deep analysis,
//! and generates optimized Claude Code agents, skills, and CLAUDE.md.

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use crate::prompts;

// ── Project scanning ────────────────────────────────────────────────────────

/// Full project profile after filesystem scan.
pub struct ProjectProfile {
    pub root: PathBuf,
    pub files_by_extension: HashMap<String, Vec<String>>,
    pub config_files: Vec<(String, String)>, // (relative path, content)
    pub sampled_code: Vec<(String, String)>, // (relative path, first 100 lines)
    pub total_files: usize,
    pub directory_tree: String,
}

/// What already exists in .claude/
pub struct ExistingArtifacts {
    pub agents: Vec<String>,
    pub skills: Vec<String>,
    pub has_claude_md: bool,
}

const IGNORE_DIRS: &[&str] = &[
    "node_modules", ".git", "target", "dist", "build", "__pycache__",
    ".next", ".nuxt", "vendor", "venv", ".venv", "env", ".env",
    ".claude", ".roo", "coverage", ".mypy_cache", ".pytest_cache",
    ".ruff_cache", ".eggs", "htmlcov",
];

const CONFIG_FILES: &[&str] = &[
    "package.json", "Cargo.toml", "pyproject.toml", "setup.py",
    "requirements.txt", "go.mod", "pom.xml", "build.gradle",
    "Gemfile", "composer.json", "tsconfig.json", "Dockerfile",
    "docker-compose.yml", "docker-compose.yaml", ".env.example",
    "jest.config.js", "jest.config.ts", "jest.config.cjs",
    "vitest.config.ts", "vitest.config.js", "vite.config.ts",
    "next.config.js", "next.config.ts", "tailwind.config.js",
    ".eslintrc.json", "eslint.config.js", "rustfmt.toml",
    ".prettierrc", "Makefile", "CMakeLists.txt",
];

/// Scan a project and build a complete profile.
pub fn scan_project(path: &Path) -> ProjectProfile {
    let mut files_by_ext: HashMap<String, Vec<String>> = HashMap::new();
    let mut config_files: Vec<(String, String)> = Vec::new();
    let mut all_files: Vec<(String, String)> = Vec::new(); // (rel_path, extension)
    let mut total_files = 0;
    let mut tree_lines: Vec<String> = Vec::new();

    walk_dir(path, path, IGNORE_DIRS, &mut |file_path, rel_path| {
        total_files += 1;
        let ext = file_path.extension()
            .map(|e| e.to_string_lossy().to_string())
            .unwrap_or_default();
        let name = file_path.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let rel_str = rel_path.to_string_lossy().to_string();

        files_by_ext.entry(ext.clone()).or_default().push(rel_str.clone());
        all_files.push((rel_str.clone(), ext.clone()));

        // Capture config files with their content
        if CONFIG_FILES.iter().any(|c| *c == name) {
            if let Ok(content) = read_first_n_lines(file_path, 100) {
                config_files.push((rel_str.clone(), content));
            }
        }

        // Build tree (simplified — just track unique directories)
        if let Some(parent) = rel_path.parent() {
            let dir = parent.to_string_lossy().to_string();
            if !dir.is_empty() && !tree_lines.contains(&dir) {
                tree_lines.push(dir);
            }
        }
    });

    // Sample code files: up to 3 per language extension
    let code_extensions = &["rs", "py", "js", "ts", "tsx", "go", "java", "rb", "php", "cs", "c", "cpp", "swift", "kt", "dart", "ex"];
    let mut sampled_code: Vec<(String, String)> = Vec::new();
    let mut sampled_per_ext: HashMap<String, usize> = HashMap::new();

    for (rel_path, ext) in &all_files {
        if !code_extensions.contains(&ext.as_str()) { continue; }
        let count = sampled_per_ext.entry(ext.clone()).or_default();
        if *count >= 3 { continue; }

        let full_path = path.join(rel_path);
        if let Ok(content) = read_first_n_lines(&full_path, 100) {
            if content.len() > 50 { // Skip near-empty files
                sampled_code.push((rel_path.clone(), content));
                *count += 1;
            }
        }
    }

    // Sort tree
    tree_lines.sort();
    let directory_tree = tree_lines.iter()
        .take(30)
        .map(|d| format!("  {}/", d))
        .collect::<Vec<_>>()
        .join("\n");

    ProjectProfile {
        root: path.to_path_buf(),
        files_by_extension: files_by_ext,
        config_files,
        sampled_code,
        total_files,
        directory_tree,
    }
}

/// Detect existing .claude/ artifacts.
pub fn detect_existing(path: &Path) -> ExistingArtifacts {
    let claude_dir = path.join(".claude");

    let agents = fs::read_dir(claude_dir.join("agents"))
        .ok()
        .map(|entries| {
            entries.flatten()
                .filter(|e| e.path().extension().map(|x| x == "md").unwrap_or(false))
                .filter_map(|e| e.path().file_stem().map(|n| n.to_string_lossy().to_string()))
                .collect()
        })
        .unwrap_or_default();

    let skills = fs::read_dir(claude_dir.join("skills"))
        .ok()
        .map(|entries| {
            entries.flatten()
                .filter(|e| e.path().is_dir())
                .filter_map(|e| e.file_name().to_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    let has_claude_md = path.join("CLAUDE.md").exists();

    ExistingArtifacts { agents, skills, has_claude_md }
}

/// Format the profile into prompt-ready strings.
impl ProjectProfile {
    pub fn ext_summary(&self) -> String {
        let mut exts: Vec<(&String, usize)> = self.files_by_extension.iter()
            .map(|(ext, files)| (ext, files.len()))
            .collect();
        exts.sort_by(|a, b| b.1.cmp(&a.1));
        exts.iter()
            .take(15)
            .map(|(ext, count)| format!("  .{}: {} files", ext, count))
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn config_summary(&self) -> String {
        self.config_files.iter()
            .map(|(path, content)| {
                let truncated = if content.len() > 500 {
                    format!("{}...", &content[..500])
                } else {
                    content.clone()
                };
                format!("--- {} ---\n{}", path, truncated)
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    pub fn code_summary(&self) -> String {
        self.sampled_code.iter()
            .take(5) // Max 5 samples in the prompt
            .map(|(path, content)| {
                let truncated = if content.len() > 800 {
                    format!("{}...", &content[..800])
                } else {
                    content.clone()
                };
                format!("--- {} ---\n{}", path, truncated)
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}

// ── AI pipeline ─────────────────────────────────────────────────────────────

/// Send a prompt to the AI via the proxy and get a response.
pub async fn call_ai(prompt: &str, proxy_port: u16) -> Result<String, String> {
    let body = serde_json::json!({
        "model": "deepseek-chat",
        "max_tokens": 8192,
        "messages": [{"role": "user", "content": prompt}]
    });

    let url = format!("http://localhost:{}/v1/messages", proxy_port);

    let body_str = body.to_string();
    let output = tokio::process::Command::new("curl")
        .args([
            "-sS",          // silent but show errors
            "--max-time", "60",
            "-X", "POST",
            &url,
            "-H", "Content-Type: application/json",
            "-H", "anthropic-version: 2023-06-01",
            "-H", "x-api-key: scan-via-proxy",
            "-d", &body_str,
        ])
        .output()
        .await
        .map_err(|e| format!("Failed to call AI: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        return Err(format!("AI call failed (exit {}): stderr={} stdout={}",
            output.status.code().unwrap_or(-1),
            stderr.chars().take(200).collect::<String>(),
            stdout.chars().take(200).collect::<String>(),
        ));
    }

    let response: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("Failed to parse AI response: {}", e))?;

    // Extract text from Anthropic Messages API response
    response.get("content")
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.first())
        .and_then(|block| block.get("text"))
        .and_then(|t| t.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "No text in AI response".to_string())
}

/// Extract JSON from AI response (handles markdown code blocks).
pub fn extract_json(response: &str) -> Result<serde_json::Value, String> {
    // Try direct parse first
    if let Ok(v) = serde_json::from_str(response) {
        return Ok(v);
    }

    // Try extracting from ```json ... ``` blocks
    if let Some(start) = response.find("```json") {
        let after = &response[start + 7..];
        if let Some(end) = after.find("```") {
            let json_str = after[..end].trim();
            if let Ok(v) = serde_json::from_str(json_str) {
                return Ok(v);
            }
        }
    }

    // Try extracting from ``` ... ``` blocks
    if let Some(start) = response.find("```") {
        let after = &response[start + 3..];
        if let Some(end) = after.find("```") {
            let json_str = after[..end].trim();
            if let Ok(v) = serde_json::from_str(json_str) {
                return Ok(v);
            }
        }
    }

    // Try finding first { ... } or [ ... ]
    if let Some(start) = response.find('{') {
        if let Some(end) = response.rfind('}') {
            let json_str = &response[start..=end];
            if let Ok(v) = serde_json::from_str(json_str) {
                return Ok(v);
            }
        }
    }

    Err(format!("Could not extract JSON from response: {}", &response[..response.len().min(200)]))
}

/// Run the full AI-powered scan pipeline.
pub async fn run_pipeline(
    profile: &ProjectProfile,
    existing: &ExistingArtifacts,
    proxy_port: u16,
) -> Result<ScanResult, String> {
    // Step 1: Analysis
    eprintln!("  [2/6] Analyzing project (via DeepSeek)...");
    let analysis_prompt = prompts::analysis_prompt(
        profile.total_files,
        &profile.ext_summary(),
        &profile.config_summary(),
        &profile.code_summary(),
    );
    let analysis_response = call_ai(&analysis_prompt, proxy_port).await?;
    let analysis = extract_json(&analysis_response)?;

    let langs = analysis["languages"].as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(", "))
        .unwrap_or_default();
    let ptype = analysis["project_type"].as_str().unwrap_or("unknown");
    let complexity = analysis["complexity"].as_str().unwrap_or("unknown");
    eprintln!("        Detected: {}", langs);
    eprintln!("        Type: {} ({})", ptype, complexity);

    // Step 2: Planning
    eprintln!("  [3/6] Planning agents and skills...");
    let existing_list = if existing.agents.is_empty() {
        "None".to_string()
    } else {
        existing.agents.join(", ")
    };
    let planning_prompt = prompts::planning_prompt(&analysis.to_string(), &existing_list);
    let planning_response = call_ai(&planning_prompt, proxy_port).await?;
    let plan = extract_json(&planning_response)?;

    let planned_agents = plan["agents"].as_array().map(|a| a.len()).unwrap_or(0);
    let planned_skills = plan["skills"].as_array().map(|a| a.len()).unwrap_or(0);
    eprintln!("        Will generate: {} agents, {} skills", planned_agents, planned_skills);

    // Step 3-5: Per-domain agent generation
    eprintln!("  [4/6] Generating domain-specific content...");
    let mut agent_outputs: Vec<String> = Vec::new();

    if let Some(agents) = plan["agents"].as_array() {
        for agent in agents {
            let name = agent["name"].as_str().unwrap_or("unknown");

            // Skip if already exists
            if existing.agents.iter().any(|a| a == name) {
                eprintln!("        Skipping {} (already exists)", name);
                continue;
            }

            let domain = agent["domain"].as_str().unwrap_or("general");
            eprint!("        ├─ {} ({})...", name, domain);

            let gen_prompt = prompts::agent_generation_prompt(
                &agent.to_string(),
                &analysis.to_string(),
                &profile.code_summary(),
            );

            match call_ai(&gen_prompt, proxy_port).await {
                Ok(output) => {
                    agent_outputs.push(format!("AGENT:{}\n{}", name, output));
                    eprintln!(" ✓");
                }
                Err(e) => {
                    eprintln!(" ✗ ({})", e);
                }
            }
        }
    }

    // Step 6: Synthesis
    eprintln!("  [5/6] Synthesizing...");
    let all_outputs = agent_outputs.join("\n\n---\n\n");
    let skills_json = plan.get("skills").map(|s| s.to_string()).unwrap_or("[]".to_string());
    let combined = format!("Agent Outputs:\n{}\n\nPlanned Skills:\n{}", all_outputs, skills_json);

    let synthesis_prompt = prompts::synthesis_prompt(&combined, &analysis.to_string());
    let synthesis_response = call_ai(&synthesis_prompt, proxy_port).await?;
    let synthesis = extract_json(&synthesis_response)?;

    let total_agents = synthesis["agents"].as_array().map(|a| a.len()).unwrap_or(0);
    let total_skills = synthesis["skills"].as_array().map(|a| a.len()).unwrap_or(0);
    eprintln!("        Merged → {} agents, {} skills", total_agents, total_skills);

    Ok(ScanResult {
        analysis,
        plan,
        synthesis,
        existing_skipped: existing.agents.clone(),
    })
}

pub struct ScanResult {
    pub analysis: serde_json::Value,
    pub plan: serde_json::Value,
    pub synthesis: serde_json::Value,
    pub existing_skipped: Vec<String>,
}

// ── File writing ────────────────────────────────────────────────────────────

/// Write all generated artifacts to disk.
pub fn write_artifacts(root: &Path, result: &ScanResult, force: bool) -> WriteSummary {
    let mut summary = WriteSummary::default();
    let claude_dir = root.join(".claude");

    // Write agents
    if let Some(agents) = result.synthesis["agents"].as_array() {
        let agents_dir = claude_dir.join("agents");
        fs::create_dir_all(&agents_dir).ok();

        for agent in agents {
            let name = agent["name"].as_str().unwrap_or("unknown");
            let content = agent["content"].as_str().unwrap_or("");
            if content.is_empty() { continue; }

            let path = agents_dir.join(format!("{}.md", name));
            if path.exists() && !force {
                eprintln!("        Skipping: {} (exists)", name);
                summary.agents_skipped += 1;
                continue;
            }

            if fs::write(&path, content).is_ok() {
                eprintln!("        Created: {}", name);
                summary.agents_created += 1;
            }
        }
    }

    // Write skills
    if let Some(skills) = result.synthesis["skills"].as_array() {
        let skills_dir = claude_dir.join("skills");

        for skill in skills {
            let name = skill["name"].as_str().unwrap_or("unknown");
            let content = skill["content"].as_str().unwrap_or("");
            if content.is_empty() { continue; }

            let skill_dir = skills_dir.join(name);
            let path = skill_dir.join("SKILL.md");
            if path.exists() && !force {
                summary.skills_skipped += 1;
                continue;
            }

            fs::create_dir_all(&skill_dir).ok();
            if fs::write(&path, content).is_ok() {
                summary.skills_created += 1;
            }
        }
    }

    // Write CLAUDE.md
    if let Some(claude_md) = result.synthesis["claude_md"].as_str() {
        if !claude_md.is_empty() {
            let path = root.join("CLAUDE.md");
            if path.exists() && !force {
                eprintln!("        Skipping: CLAUDE.md (exists)");
                summary.claude_md_skipped = true;
            } else {
                if fs::write(&path, claude_md).is_ok() {
                    eprintln!("        Created: CLAUDE.md");
                    summary.claude_md_created = true;
                }
            }
        }
    }

    summary
}

#[derive(Default)]
pub struct WriteSummary {
    pub agents_created: usize,
    pub agents_skipped: usize,
    pub skills_created: usize,
    pub skills_skipped: usize,
    pub claude_md_created: bool,
    pub claude_md_skipped: bool,
}

// ── Helpers ─────────────────────────────────────────────────────────────────

fn walk_dir(
    dir: &Path,
    root: &Path,
    ignore: &[&str],
    callback: &mut dyn FnMut(&Path, &Path),
) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();

        if ignore.iter().any(|i| *i == name) { continue; }
        if name.starts_with('.') && name != ".env.example" { continue; }

        if path.is_dir() {
            walk_dir(&path, root, ignore, callback);
        } else if path.is_file() {
            let rel = path.strip_prefix(root).unwrap_or(&path);
            callback(&path, rel);
        }
    }
}

fn read_first_n_lines(path: &Path, n: usize) -> Result<String, std::io::Error> {
    let file = fs::File::open(path)?;
    let reader = BufReader::new(file);
    let lines: Vec<String> = reader.lines()
        .take(n)
        .filter_map(|l| l.ok())
        .collect();
    Ok(lines.join("\n"))
}
