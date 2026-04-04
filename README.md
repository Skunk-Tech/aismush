# AISmush

**Cut your AI coding costs by 90%+ without sacrificing quality.**

AISmush is a transparent proxy that sits between Claude Code and the AI providers. It intelligently routes each turn to the best model for the job — Claude for complex reasoning, DeepSeek or OpenRouter for moderate tasks, local models (Ollama, LM Studio, etc.) for free mechanical work — while compressing context, managing memory across sessions, and tracking every dollar saved.

One binary. One setup command. Drop-in replacement that works with your existing Claude Code setup.

## The Problem

Claude Code is incredible, but it's expensive. A heavy coding session can burn through $50-100+ in API costs. Most of those tokens are spent on mechanical tasks — reading files, processing tool results, making simple edits — that don't need Claude's full reasoning power.

## The Solution

AISmush automatically detects what kind of work each turn requires and routes to the cheapest model that can handle it:

| Turn Type | Tier | Routed To | Why |
|-----------|------|-----------|-----|
| Planning, architecture | Premium | **Claude** | Complex reasoning matters here |
| Error recovery, debugging | Premium | **Claude** | Needs deep analysis to break out |
| Code generation | Mid | **DeepSeek / OpenRouter** | Following an established plan |
| Tool result processing | Free | **Ollama / local model** | Mechanical — zero cost |
| File reads, simple edits | Free | **Ollama / local model** | No reasoning needed |
| High blast-radius files | Premium | **Claude** | Shared types need careful handling |
| Large context (>64K tokens) | Premium | **Claude** | Beyond smaller model windows |

**Real-world result: 90%+ cost savings.** With local models handling tool results, most turns cost nothing.

## Features

### AI-Powered Project Scanner (`aismush --scan`)
- Scans your codebase, reads actual code, sends to AI for deep analysis
- Full pipeline: Analyze → Plan → Generate per-domain → Synthesize (5-7 AI calls)
- Generates agents deeply customized to YOUR code patterns, conventions, and architecture
- Creates `.claude/agents/`, `.claude/skills/`, and `CLAUDE.md` — all project-specific
- Each agent assigned the optimal model: Haiku for cheap tasks, Sonnet for complex work
- Detects existing `.claude/` files and skips them (use `--force` to overwrite)
- Auto-starts proxy if not running — truly one command
- **Incremental scanning** — SHA-256 hashes every file, re-scans only what changed
- First scan: ~$0.03. Re-scans: ~$0.003 (90% cheaper)

### Structural Summarization — 3-5x Token Reduction
The single biggest token saver. Older tool results in your conversation get replaced with compact structural summaries — function signatures, type definitions, and imports — while your recent work stays fully intact.

- A 200-line file in an old message becomes ~30 lines (function signatures + types only)
- **3-5x reduction** on code tool_results older than the last 4 messages
- Never touches JSON, YAML, or data — only code gets summarized
- Never touches error results — full context preserved for debugging
- Never touches your last 4 messages — active work stays raw
- Saves **thousands of tokens per request** in long sessions

### Smart Multi-Provider Routing + Blast-Radius Analysis
- Routes each turn across **Claude, DeepSeek, OpenRouter (290+ models), and local servers** (Ollama, LM Studio, llama.cpp, vLLM, Jan, KoboldCpp)
- **Tier-based routing** — Free (local), Budget, Mid (DeepSeek/OpenRouter), Premium (Claude), Ultra (Opus)
- **Blast-radius aware** — parses your project's import graph to know which files are shared by many others
- Editing a leaf file? Local model handles it free. Editing a type definition imported by 12 files? Claude.
- **Auto-discovery** — detects running local servers on known ports automatically
- **Fallback chains** — if local model is down, falls back to cloud; if DeepSeek is down, falls back to Claude
- Context-size aware — forces Claude when context exceeds smaller model windows
- Error recovery detection — escalates to Claude when cheaper models are going in circles

### Context Compression
- **Content-type aware** (RTK-inspired) — detects Code, Data (JSON/YAML/XML), Logs, Unknown
- Structural summarization for old code (see above)
- Strips comments from code (never touches JSON, YAML, or data formats)
- Normalizes whitespace, collapses blank lines
- Aggressively deduplicates repeated log lines
- Smart truncation preserves function signatures in code
- **60-80% token reduction** on code (structural summaries + compression combined), safe passthrough for data

### 4-Tier Context Window Management
Prevents DeepSeek from choking on long conversations:

| Context Size | Strategy |
|---|---|
| < 55K tokens | No intervention — both providers handle this fine |
| 55K – 64K | Truncate old tool results to fit DeepSeek |
| 64K+ | Route to Claude (200K window, no trimming needed) |

For DeepSeek turns only, old tool results are truncated and messages trimmed if needed. Claude turns are never trimmed — this preserves tool_use/tool_result pairs that Claude requires.

### Deep Memory — Full Conversation Capture & Search
- **Captures every conversation** — user questions, AI responses, tool invocations, reasoning
- **Local semantic search** — MiniLM-L6-v2 runs on your machine in ~10ms, finds conversations by meaning
- "auth bug" finds conversations about "JWT validation" — semantic, not just keywords
- **FTS5 keyword search** as supplement for exact matches
- **Auto-injects relevant past context** into new sessions
- **Searchable via dashboard, CLI, and API** — `aismush --search "how did I fix the auth bug"`
- Configurable retention (30 days default, or keep forever)
- ~3KB per turn, ~260MB/year at heavy usage
- **Your AI remembers everything — questions, answers, decisions, reasoning**

### Cost Tracking
- Real-time cost calculation per request
- "All-Claude equivalent" comparison — see what you would have paid
- Savings percentage (session + cumulative)
- Full request history with provider, model, tokens, latency

### Live Dashboard
Open `http://localhost:1849/dashboard` to see:
- **Compression savings** — tokens saved by stripping comments/dedup (works in ALL modes, including direct)
- **Routing savings** — money saved by sending turns to DeepSeek (smart routing mode)
- In direct mode, shows how much you *could* save by enabling smart routing
- Request counts, routing distribution
- Recent requests table with per-request cost breakdown
- Memory viewer with search and delete
- Request history with full detail

### Plan Orchestrator — Say "Go" and Walk Away
Built-in autonomous plan execution. Ask Claude to make a plan, then say **"run plan"** — AISmush analyzes the steps, maps each one to the best specialized agent (rust-expert, data-engineer, frontend-engineer, etc.), figures out which steps can run in parallel, and executes the entire thing end-to-end. No custom file formats, no CLI commands — just talk to Claude.

- Reads plans Claude already generates (standard markdown)
- Automatically maps steps to your project's specialized agents
- Runs independent steps in parallel for speed
- Passes context forward so each agent knows what prior steps accomplished
- Verifies results (cargo check, cargo test) after completion
- Confirms before executing — you always have the final say

### Lightweight by Default
- Written in Rust with Tokio + Hyper (same stack as Cloudflare's infrastructure)
- **~15 MB memory** with embeddings disabled (default) — won't slow down your machine
- Embeddings (semantic search) are opt-in: `aismush --embeddings` or `AISMUSH_EMBEDDINGS=1`
- Frame-by-frame response streaming — feels identical to direct Claude connection
- Connection pooling with keep-alive (no TLS handshake per request)

## Three Modes

**Smart Routing** (default) — Routes between Claude, DeepSeek, OpenRouter, and local models. Max savings. Run `aismush --setup` to configure your providers.
```bash
aismush-start
```

**Local + Cloud** — Local models handle free tasks, cloud handles the rest. Best for users running Ollama or LM Studio. AISmush auto-discovers local servers on startup.
```bash
aismush-start  # with Ollama running on port 11434
```

**Direct Mode** — Claude only, no other providers needed. You still get compression, memory, agents, context management, and cost tracking. The dashboard shows what compression saves you.
```bash
aismush-start --direct
```

All modes give you AI-generated project agents, persistent memory, context compression, and the full dashboard.

## Quick Start

### One-Line Install (Linux / macOS)

```bash
curl -fsSL https://raw.githubusercontent.com/Skunk-Tech/aismush/main/install.sh | bash
```

This downloads the right binary for your platform and installs it to `~/.local/bin/`.

### Run

```bash
aismush-start
```

First run offers interactive setup (`aismush --setup`) to configure DeepSeek, OpenRouter, and/or local models — with connection testing for each. Or just paste a DeepSeek key for quick start.

That's it. Two commands total: install, then run.

### One-Line Install (Windows)

```powershell
irm https://raw.githubusercontent.com/Skunk-Tech/aismush/main/install.ps1 | iex
```

This downloads the binary, installs it to `%LOCALAPPDATA%\AISmush\`, and adds it to your PATH.

### Run

```powershell
aismush-start
```

First run offers interactive setup or quick DeepSeek key entry, same as Linux/macOS.

### Manual Download

| Platform | File |
|----------|------|
| Linux x86_64 | `aismush-linux-x86_64.tar.gz` |
| macOS Intel | `aismush-macos-x86_64.tar.gz` |
| macOS Apple Silicon | `aismush-macos-arm64.tar.gz` |
| Windows | `aismush-windows-x86_64.tar.gz` |

### Build from Source

```bash
git clone https://github.com/Skunk-Tech/aismush.git
cd aismush
cargo build --release
cp target/release/aismush ~/.local/bin/
```

## How It Works

```
You type in Claude Code
    |
    v
Claude Code sends request to AISmush (localhost:1849)
    |
    v
AISmush analyzes the turn:
    |-- Classify task type (planning, debugging, tool result, etc.)
    |-- Check blast-radius of files being edited
    |-- Compress tool_result content (strip comments, dedup, truncate)
    |-- Estimate token count
    |-- Inject memories from previous sessions
    |-- Pick cheapest healthy provider at the required tier
    |
    +---> Claude API (planning, debugging, high blast-radius, large context)
    +---> DeepSeek API (code generation, moderate complexity)
    +---> OpenRouter (290+ models, fallback option)
    +---> Local model (Ollama etc. — tool results, file reads — FREE)
              |
              +---> Response streams back to Claude Code
    |
    v
AISmush logs: provider, tokens, cost, latency → SQLite
Extracts memorable facts from response → SQLite
```

**Claude Code doesn't know the difference.** It sends requests to `localhost:1849` instead of `api.anthropic.com`, and everything else works identically.

## Configuration

AISmush reads config from (in priority order):
1. Environment variables
2. `config.json` or `.deepseek-proxy.json` in the current directory
3. `~/.hybrid-proxy/config.json`

### Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `DEEPSEEK_API_KEY` | (none) | Your DeepSeek API key |
| `OPENROUTER_API_KEY` | (none) | Your OpenRouter API key |
| `LOCAL_MODEL_URL` | (none) | Local model server URL (e.g. `http://localhost:11434`) |
| `LOCAL_MODEL_NAME` | (none) | Local model name (e.g. `qwen3:8b`) |
| `PROXY_PORT` | `1849` | Port for the proxy server |
| `PROXY_VERBOSE` | `false` | Enable debug logging |
| `FORCE_PROVIDER` | (none) | Force all requests to a specific provider |
| `AISMUSH_AUTO_DISCOVER` | `true` | Auto-discover local model servers |
| `AISMUSH_BLAST_THRESHOLD` | `0.5` | Blast-radius score threshold for tier escalation |

### Config File

```json
{
  "apiKey": "sk-your-deepseek-key",
  "openrouterKey": "sk-or-your-key",
  "local": [
    {"name": "ollama", "url": "http://localhost:11434", "model": "qwen3:8b"}
  ],
  "routing": {
    "blastRadiusThreshold": 0.5,
    "preferLocal": true,
    "minTierForPlanning": "premium",
    "minTierForDebugging": "mid"
  },
  "port": 1849,
  "verbose": false,
  "forceProvider": null
}
```

### Claude Authentication

**You don't need to configure Claude's API key.** Claude Code sends its own authentication headers with every request, and AISmush passes them through transparently. Only DeepSeek needs a separate key (because the proxy authenticates on your behalf).

## API Endpoints

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/health` | GET | Health check |
| `/stats` | GET | Aggregated statistics (JSON) |
| `/history` | GET | Recent request log (JSON) |
| `/memories` | GET | All stored memories (JSON) |
| `/memories/clear` | POST | Delete all memories |
| `/dashboard` | GET | Live HTML dashboard |

## Data Storage

All data stays on your machine:

```
~/.hybrid-proxy/
├── proxy.db     ← SQLite database (requests, sessions, memories)
└── proxy.log    ← Proxy log output
```

## Cost Comparison

Based on real usage with a large Rust/React/Node.js codebase:

| Metric | Without AISmush | With AISmush |
|--------|----------------|--------------|
| Planning turns (Claude) | $15/M in, $75/M out | $15/M in, $75/M out (same) |
| Coding turns (Claude) | $15/M in, $75/M out | **$0.27/M in, $1.10/M out** |
| Context tokens | 100% | **20-40%** (structural summaries + compression) |
| Typical session cost | $20-50 | **$1.50-4** |
| **Savings** | — | **92%+** |

## Architecture

```
19 Rust modules:

main.rs        — HTTP server + request pipeline
config.rs      — JSON + env config loading
provider.rs    — Provider abstraction (tiers, health, registry)
router.rs      — Multi-factor tier-based routing engine
forward.rs     — Claude/DeepSeek/OpenAI-compat forwarding + streaming
transform.rs   — Anthropic ↔ OpenAI format conversion
discovery.rs   — Auto-discovery of local model servers
setup.rs       — Interactive provider setup with connection testing
compress.rs    — Context compression engine
summarize.rs   — Structural code summarization (3-5x token reduction)
hashes.rs      — SHA-256 file hashing + incremental change detection
deps.rs        — Dependency graph + blast-radius analysis
context.rs     — Provider-aware context window management
memory.rs      — Cross-session persistent memory
tokens.rs      — Token estimation
cost.rs        — Dynamic pricing + compression-aware cost calculation
db.rs          — SQLite persistence layer (with date filtering)
dashboard.rs   — Live HTML dashboard with date range filtering
state.rs       — Shared state types + provider registry
```

**Dependencies:** tokio, hyper, rustls, serde, rusqlite (bundled). No OpenSSL, no Node.js, no runtime dependencies.

## FAQ

**Q: Does this affect response quality?**
A: For planning and complex reasoning — no, those still go to Claude. For mechanical coding (processing tool results, simple edits) — DeepSeek V3 is excellent and often indistinguishable from Claude for these tasks.

**Q: Can I force all requests to one provider?**
A: Yes. Set `FORCE_PROVIDER=claude` (no savings, full Claude) or `FORCE_PROVIDER=deepseek` (max savings).

**Q: Does Claude Code know it's being proxied?**
A: No. It sends requests to localhost instead of api.anthropic.com, but the API format is identical. All Claude Code features work normally.

**Q: What if DeepSeek is down?**
A: DeepSeek 500/502 errors return immediately with a clear message. Set `FORCE_PROVIDER=claude` to bypass.

**Q: What if Claude is down or rate-limited?**
A: The proxy automatically falls back to DeepSeek — it trims context to fit DeepSeek's window and sends the request there. Your work is never blocked. Both providers have to be down simultaneously for a request to fail.

**Q: Is my data sent anywhere?**
A: Your requests go to the same APIs you'd normally use (Anthropic + DeepSeek). The proxy runs locally — no third-party servers, no telemetry, no data collection.

**Q: Does it work with Claude Code in VS Code?**
A: Yes. Set `ANTHROPIC_BASE_URL=http://localhost:1849` in your VS Code settings, or use `start.sh` / `aismush-start` which handles this automatically.

**Q: Does it work with the Anthropic API / SDK directly?**
A: Yes. Any application that uses the Anthropic Messages API works — just set `ANTHROPIC_BASE_URL=http://localhost:1849`. This includes the Anthropic Python/TypeScript SDKs, Claude Code CLI, Claude Code VS Code extension, and any custom integration.

**Q: How do I check if the proxy is running?**
A: Run `aismush --status` from any terminal, or visit `http://localhost:1849/dashboard`.

## CLI Reference

```bash
aismush              # Start the proxy server (smart routing, lightweight)
aismush --direct     # Start in direct mode (Claude only, still compresses + tracks)
aismush --setup      # Interactive provider setup (DeepSeek, OpenRouter, local models)
aismush --providers  # List all configured and discovered providers with health status
aismush --embeddings # Start with semantic search enabled (loads 90MB model)
aismush --scan       # Scan codebase, generate optimized agents
aismush --search "query"  # Search past conversations by meaning
aismush --version    # Show version
aismush --status     # Check if running, show quick stats
aismush --config     # Show current configuration
aismush --upgrade    # Upgrade to latest version
aismush --uninstall  # Uninstall AISmush
aismush --help       # Show help

aismush-start        # Start proxy + launch Claude Code (recommended)
```

## Website

**[aismush.us.com](https://aismush.us.com/)** — Feature overview, install instructions, and documentation.

## Requirements

- [Claude Code](https://claude.ai/code) CLI installed (or any Anthropic API client)
- At least one of: [DeepSeek API key](https://platform.deepseek.com/api_keys) (free tier), [OpenRouter API key](https://openrouter.ai/keys), or a local model server (Ollama, LM Studio, etc.)
- Or just use `--direct` mode with Claude only (compression + memory still work)
- Linux x86_64, macOS (Intel or Apple Silicon), or Windows x86_64

## Uninstall

```bash
aismush --uninstall
```

This works on all platforms. It removes the binary, cleans up PATH (Windows), and optionally deletes your data directory.

## License

MIT License. See [LICENSE](LICENSE).

## Author

Created by Garret Acott / [Skunk Tech](https://github.com/Skunk-Tech)
