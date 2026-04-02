# AISmush

**Cut your AI coding costs by 90%+ without sacrificing quality.**

AISmush is a transparent proxy that sits between Claude Code and the AI providers. It intelligently routes each turn to the best model for the job — Claude for complex reasoning, DeepSeek for mechanical coding — while compressing context, managing memory across sessions, and tracking every dollar saved.

One binary. Zero config. Drop-in replacement that works with your existing Claude Code setup.

## The Problem

Claude Code is incredible, but it's expensive. A heavy coding session can burn through $50-100+ in API costs. Most of those tokens are spent on mechanical tasks — reading files, processing tool results, making simple edits — that don't need Claude's full reasoning power.

## The Solution

AISmush automatically detects what kind of work each turn requires:

| Turn Type | Routed To | Why |
|-----------|-----------|-----|
| Initial planning, architecture | **Claude** | Complex reasoning matters here |
| Error recovery, debugging loops | **Claude** | Needs deep analysis to break out |
| Tool result processing | **DeepSeek** | Mechanical — read result, decide next action |
| Mid-session coding | **DeepSeek** | Following an established plan |
| Large context (>64K tokens) | **Claude** | Beyond DeepSeek's effective window |

**Real-world result: 90%+ cost savings** on a large multi-language codebase (2000+ files across Rust, React, and Node.js).

## Features

### AI-Powered Project Scanner (`aismush --scan`)
- Scans your codebase, reads actual code, sends to AI for deep analysis
- Full pipeline: Analyze → Plan → Generate per-domain → Synthesize (5-7 AI calls)
- Generates agents deeply customized to YOUR code patterns, conventions, and architecture
- Creates `.claude/agents/`, `.claude/skills/`, and `CLAUDE.md` — all project-specific
- Each agent assigned the optimal model: Haiku for cheap tasks, Sonnet for complex work
- Detects existing `.claude/` files and skips them (use `--force` to overwrite)
- Auto-starts proxy if not running — truly one command
- Costs ~$0.03 per scan via DeepSeek through the proxy

### Smart Model Routing
- Automatically routes each turn to Claude or DeepSeek based on the task
- Heuristic-based (zero latency overhead — no extra API calls for classification)
- Context-size aware — forces Claude when context exceeds DeepSeek's effective window
- Error recovery detection — switches to Claude when DeepSeek is going in circles

### Context Compression
- **Content-type aware** (RTK-inspired) — detects Code, Data (JSON/YAML/XML), Logs, Unknown
- Strips comments from code (never touches JSON, YAML, or data formats)
- Normalizes whitespace, collapses blank lines
- Aggressively deduplicates repeated log lines
- Smart truncation preserves function signatures in code
- **20-50% token reduction** on code, safe passthrough for data

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

### Zero Overhead Streaming
- Written in Rust with Tokio + Hyper (same stack as Cloudflare's infrastructure)
- Frame-by-frame response streaming — feels identical to direct Claude connection
- Connection pooling with keep-alive (no TLS handshake per request)
- ~3.5 MB binary, ~15 MB memory usage

## Two Modes

**Smart Routing** (default) — Routes between Claude and DeepSeek. Max savings (~90%). Requires a DeepSeek API key.
```bash
aismush-start
```

**Direct Mode** — Claude only, no DeepSeek needed. You still get compression, memory, agents, context management, and cost tracking. The dashboard shows how much you *could* save by enabling smart routing.
```bash
aismush-start --direct
```

Both modes give you AI-generated project agents, persistent memory, context compression, and the full dashboard.

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

First run asks for your DeepSeek API key (free at [platform.deepseek.com](https://platform.deepseek.com/api_keys)). It saves the key automatically — you'll never be asked again.

That's it. Two commands total: install, then run.

### Windows

Download from the [Releases page](https://github.com/Skunk-Tech/aismush/releases), extract, and run:

```powershell
.\start.ps1
```

First run prompts for your DeepSeek API key and saves it automatically, same as Linux/macOS.

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
    |-- Compress tool_result content (strip comments, dedup, truncate)
    |-- Estimate token count
    |-- Check context window tier
    |-- Inject memories from previous sessions
    |-- Decide: Claude or DeepSeek?
    |
    +---> Claude API (planning, debugging, large context)
    |         |
    |         +---> Response streams back to Claude Code
    |
    +---> DeepSeek API (tool results, mechanical coding)
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
| `DEEPSEEK_API_KEY` | (required) | Your DeepSeek API key |
| `PROXY_PORT` | `1849` | Port for the proxy server |
| `PROXY_VERBOSE` | `false` | Enable debug logging |
| `FORCE_PROVIDER` | (none) | Force all requests to `claude` or `deepseek` |

### Config File

```json
{
  "apiKey": "sk-your-deepseek-key",
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
| Context tokens | 100% | **50-80%** (after compression) |
| Typical session cost | $20-50 | **$2-5** |
| **Savings** | — | **90%+** |

## Architecture

```
12 Rust modules, 2,112 lines total:

main.rs        — HTTP server + request pipeline
config.rs      — JSON + env config loading
router.rs      — Smart routing heuristics
forward.rs     — Claude/DeepSeek forwarding + streaming
compress.rs    — Context compression engine
context.rs     — 4-tier context window management
memory.rs      — Cross-session persistent memory
tokens.rs      — Token estimation + SSE parsing
cost.rs        — Pricing tables + cost calculation
db.rs          — SQLite persistence layer
dashboard.rs   — Live HTML dashboard
state.rs       — Shared state types
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
aismush              # Start the proxy server (smart routing)
aismush --direct     # Start in direct mode (Claude only, still compresses + tracks)
aismush --scan       # Scan codebase, generate optimized agents
aismush --search "query"  # Search past conversations by meaning
aismush --version    # Show version
aismush --status     # Check if running, show quick stats
aismush --config     # Show current configuration
aismush --upgrade    # Upgrade to latest version
aismush --help       # Show help

aismush-start        # Start proxy + launch Claude Code (recommended)
```

## Global Savings Dashboard

See how much the AISmush community is saving worldwide: **[aismush.us.com](https://aismush.us.com/)**

Anonymous usage stats (request counts and savings only — no personal data, no code, no API keys) are reported when a session ends. This powers the live global counter on the dashboard.

## Requirements

- [Claude Code](https://claude.ai/code) CLI installed (or any Anthropic API client)
- [DeepSeek API key](https://platform.deepseek.com/api_keys) (free tier available)
- Linux x86_64, macOS (Intel or Apple Silicon), or Windows x86_64

## Uninstall

```bash
rm ~/.local/bin/aismush ~/.local/bin/aismush-start
rm -rf ~/.hybrid-proxy/
```

## License

MIT License. See [LICENSE](LICENSE).

## Author

Created by Garret Acott / [Skunk Tech](https://github.com/Skunk-Tech)
