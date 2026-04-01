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
| Large context (>50K tokens) | **Claude** | DeepSeek's window can't handle it |

**Real-world result: 90%+ cost savings** on our first project (CTR Booster V2, a 2000+ file Rust/React/Node.js codebase).

## Features

### Smart Model Routing
- Automatically routes each turn to Claude or DeepSeek based on the task
- Heuristic-based (zero latency overhead — no extra API calls for classification)
- Context-size aware — forces Claude when context exceeds DeepSeek's effective window
- Error recovery detection — switches to Claude when DeepSeek is going in circles

### Context Compression
- Strips comments from code in tool results (biggest single savings)
- Normalizes whitespace (reduces indentation, collapses blank lines)
- Deduplicates consecutive similar lines (repeated errors/warnings)
- Truncates oversized tool results (>12K chars)
- **20-50% token reduction** on typical tool results

### 4-Tier Context Window Management
Prevents DeepSeek from choking on long conversations:

| Context Size | Strategy |
|---|---|
| < 40K tokens | Pass through as-is |
| 40K – 60K | Aggressive compression (tighter truncation) |
| 60K – 120K | Force route to Claude + compress |
| > 120K | Trim oldest messages, preserving recent context |

### Persistent Memory
- Extracts key facts from AI responses (architecture decisions, bug fixes, project changes)
- Stores in local SQLite database
- Injects relevant memories into new sessions automatically
- Memories decay over time — recent, frequently-accessed facts surface first
- **Your AI remembers what you worked on yesterday**

### Cost Tracking
- Real-time cost calculation per request
- "All-Claude equivalent" comparison — see what you would have paid
- Savings percentage (session + cumulative)
- Full request history with provider, model, tokens, latency

### Live Dashboard
Open `http://localhost:1849/dashboard` to see:
- Request counts, routing distribution
- Cost savings bar with actual vs all-Claude comparison
- Recent requests table
- Memory viewer with search and delete
- Request history with full detail

### Zero Overhead Streaming
- Written in Rust with Tokio + Hyper (same stack as Cloudflare's infrastructure)
- Frame-by-frame response streaming — feels identical to direct Claude connection
- Connection pooling with keep-alive (no TLS handshake per request)
- ~3.5 MB binary, ~15 MB memory usage

## Quick Start

### Download

Grab the latest binary for your platform from [Releases](https://github.com/Skunk-Tech/aismush/releases):

- `aismush-linux-x86_64.tar.gz` — Linux
- `aismush-macos-x86_64.tar.gz` — macOS (Intel)
- `aismush-macos-arm64.tar.gz` — macOS (Apple Silicon)
- `aismush-windows-x86_64.tar.gz` — Windows

### Install

```bash
# Linux / macOS
tar xzf aismush-*.tar.gz
chmod +x aismush start.sh

# Set your DeepSeek API key
export DEEPSEEK_API_KEY=sk-your-key
# Or create config.json: {"apiKey": "sk-your-key"}

# Run
./start.sh
```

```powershell
# Windows
tar xzf aismush-windows-x86_64.tar.gz
$env:DEEPSEEK_API_KEY = "sk-your-key"
.\start.ps1
```

That's it. AISmush starts the proxy, launches Claude Code pointed at it, and everything works transparently.

### Build from Source

```bash
# Requires Rust 1.70+
git clone https://github.com/Skunk-Tech/aismush.git
cd aismush
cargo build --release
# Binary at target/release/aismush
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
A: Requests that fail on DeepSeek will error. In that case, set `FORCE_PROVIDER=claude` to bypass DeepSeek entirely.

**Q: Is my data sent anywhere?**
A: Your requests go to the same APIs you'd normally use (Anthropic + DeepSeek). The proxy runs locally — no third-party servers, no telemetry, no data collection.

**Q: Does it work with Claude Code in VS Code?**
A: Yes. Set `ANTHROPIC_BASE_URL=http://localhost:1849` in your VS Code settings, or use `start.sh` which handles this automatically.

## Requirements

- [Claude Code](https://claude.ai/code) CLI installed
- [DeepSeek API key](https://platform.deepseek.com/api_keys) (free tier available)
- Linux x86_64, macOS (Intel or Apple Silicon), or Windows x86_64

## License

MIT License. See [LICENSE](LICENSE).

## Author

Created by Garret Acott / [Skunk Tech](https://github.com/Skunk-Tech)
