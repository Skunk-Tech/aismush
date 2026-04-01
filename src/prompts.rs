//! AI prompt templates for the project scanner.
//!
//! Each function builds a prompt for a specific stage of the pipeline.
//! Prompts request JSON responses for structured parsing.

/// Build the initial analysis prompt from scan results.
pub fn analysis_prompt(
    total_files: usize,
    ext_summary: &str,
    config_contents: &str,
    code_samples: &str,
) -> String {
    format!(r#"Analyze this codebase and provide structured information about it.

Total Files: {total_files}

Files by Extension:
{ext_summary}

Configuration Files:
{config_contents}

Code Samples:
{code_samples}

Based on this information, identify:
1. Primary and secondary programming languages
2. Frameworks and libraries in use (be specific — not just "web framework" but "Axum" or "React 18")
3. Build tools and package managers
4. Testing frameworks
5. Overall project type (web app, CLI tool, library, microservice, etc.)
6. Complexity level (simple, moderate, complex)
7. Key architectural patterns you can identify from the code samples
8. Notable conventions (naming, file organization, error handling approach)

Respond with ONLY valid JSON, no other text:
{{
    "languages": ["Rust", "TypeScript"],
    "frameworks": ["Axum", "React", "Tokio"],
    "tools": ["cargo", "npm", "vitest"],
    "testing": ["cargo test", "vitest"],
    "project_type": "fullstack_web_application",
    "complexity": "complex",
    "patterns": ["event-driven architecture", "state machine for identity lifecycle"],
    "conventions": ["snake_case for Rust", "PascalCase for React components"]
}}"#)
}

/// Build the planning prompt to decide which agents/skills to generate.
pub fn planning_prompt(analysis_json: &str, existing_agents: &str) -> String {
    format!(r#"Based on this project analysis, determine which Claude Code agents and skills should be generated.

Project Analysis:
{analysis_json}

Already existing agents (DO NOT regenerate these):
{existing_agents}

Available agent types to generate:
- Language experts (one per major language: rust-expert, typescript-expert, python-expert, etc.)
- Debugger (language-aware debugging specialist)
- Code reviewer (read-only, uses haiku model for cost efficiency)
- Test runner (testing specialist, uses haiku for cost)
- Explorer (fast codebase search, always haiku model)
- Frontend engineer (if web UI exists)
- Backend engineer (if server/API exists)
- DevOps engineer (if Docker/CI detected)
- Security reviewer (if auth/crypto patterns found)
- Data engineer (if database/migrations found)

Available skill types:
- Build/compile commands
- Test commands
- Lint/format commands
- Deploy commands
- Common development workflows (TDD cycle, PR workflow, etc.)

For each agent, specify:
- name (kebab-case)
- description (when Claude should delegate to this agent)
- model: "haiku" for read-only/cheap tasks, "sonnet" for balanced, "opus" for complex reasoning
- tools needed
- priority (high/medium/low)

For each skill, specify:
- name (kebab-case)
- description
- commands involved

Respond with ONLY valid JSON:
{{
    "agents": [
        {{
            "name": "rust-expert",
            "description": "Rust systems programming specialist for this Axum/Tokio project",
            "model": "sonnet",
            "tools": ["Read", "Edit", "Write", "Bash", "Grep", "Glob"],
            "priority": "high",
            "domain": "language"
        }}
    ],
    "skills": [
        {{
            "name": "run-tests",
            "description": "Run the project test suite",
            "commands": ["cargo test --workspace --lib"]
        }}
    ],
    "workflows": [
        {{
            "name": "tdd-cycle",
            "description": "Test-Driven Development workflow",
            "steps": ["Write test", "Run test (expect fail)", "Implement", "Run test (expect pass)", "Refactor"]
        }}
    ]
}}"#)
}

/// Build a domain-specific agent generation prompt.
pub fn agent_generation_prompt(
    agent_spec: &str,
    analysis_json: &str,
    relevant_samples: &str,
) -> String {
    format!(r#"Generate a detailed Claude Code agent definition for this project.

Agent Specification:
{agent_spec}

Project Analysis:
{analysis_json}

Relevant Code Samples:
{relevant_samples}

Generate a complete agent markdown file with YAML frontmatter. The agent should be deeply customized to THIS project — reference specific files, patterns, conventions, and tools found in the code samples.

The agent must be genuinely useful, not generic. Include:
- Project-specific patterns and conventions
- Key files and directories relevant to this agent's domain
- Common tasks this agent will handle
- Specific commands and workflows
- Known pitfalls or gotchas from the codebase

Respond with ONLY the complete markdown content (including the --- frontmatter delimiters). Example format:

---
name: agent-name
description: When Claude should use this agent
tools: Read, Edit, Write, Bash, Grep, Glob
model: sonnet
---

You are a [role] working on [this specific project].

[Detailed, project-specific instructions...]
"#)
}

/// Build the synthesis prompt to merge all domain outputs.
pub fn synthesis_prompt(all_outputs: &str, analysis_json: &str) -> String {
    format!(r##"Merge these agent generation outputs into a cohesive set of artifacts for a Claude Code project.

Project Analysis:
{analysis_json}

Generated Outputs:
{all_outputs}

Your tasks:
1. Deduplicate any overlapping agents or skills
2. Ensure agent descriptions clearly distinguish when each should be used
3. Generate a CLAUDE.md overview that covers:
   - Project description (what it does, tech stack)
   - Quick start commands (build, test, run)
   - Key architectural patterns
   - Important conventions
   - Common development workflows
4. Generate any additional skills for common operations
5. Resolve any conflicts between agents

Respond with ONLY valid JSON:
{{
    "agents": [
        {{
            "name": "agent-name",
            "content": "---\nname: agent-name\ndescription: ...\ntools: Read, Edit\nmodel: sonnet\n---\n\nFull agent prompt here..."
        }}
    ],
    "skills": [
        {{
            "name": "skill-name",
            "content": "---\nname: skill-name\ndescription: ...\n---\n\nSkill content here..."
        }}
    ],
    "claude_md": "# Project Name\n\nFull CLAUDE.md content here...",
    "summary": {{
        "total_agents": 5,
        "total_skills": 8,
        "project_type": "fullstack web application"
    }}
}}"##)
}
