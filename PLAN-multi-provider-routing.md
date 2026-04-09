# Plan: OpenRouter, Local Models, and Intelligent Multi-Tier Routing

## Context

AISmush currently only routes between Claude and DeepSeek with a simple binary heuristic. The blast-radius system exists but only does a crude override (score > 0.7 flips DeepSeek to Claude). We need to support OpenRouter (290+ models via one API key), local model servers (Ollama, LM Studio, llama.cpp, vLLM, etc.), and build genuinely intelligent routing that considers task type, blast radius, model capability, context window size, cost, and provider health.

**Critical finding**: Every major local model server AND OpenRouter all expose the same OpenAI-compatible `/v1/chat/completions` endpoint. This means one unified forwarding code path handles all non-Claude providers.

---

## Phase 1: Provider Abstraction (`src/provider.rs` — new file)

Core types that everything else builds on:

```
ProviderKind: Anthropic | OpenAICompat
Tier: Free | Budget | Mid | Premium | Ultra (implements Ord)
HealthState: Healthy | Degraded { error_count } | Down { since }

ProviderConfig {
    id: String,              // "claude", "deepseek", "openrouter", "ollama-qwen3"
    kind: ProviderKind,
    base_url: String,
    api_key: Option<String>,
    default_model: String,
    context_window: usize,   // tokens
    tier: Tier,
    pricing: (f64, f64),     // input/output per M tokens (0/0 for local)
    max_output_tokens: usize,
    supports_tools: bool,    // many local models can't handle tool_use
    health: RwLock<HealthState>,
}

ProviderRegistry {
    providers: Vec<ProviderConfig>,
    // Methods: at_tier(), cheapest_healthy_at_tier(), fallback_chain(), mark_down(), mark_healthy()
}
```

Wire `ProviderRegistry` into `ProxyState`. Register Claude + DeepSeek at startup — zero behavior change initially.

---

## Phase 2: Configuration Expansion (`src/config.rs`)

New fields on `ProxyConfig` (all additive, backward-compatible):

- `openrouter_api_key: String` — from `OPENROUTER_API_KEY` env or config JSON
- `local_servers: Vec<{name, url, model}>` — explicit local server configs
- `auto_discover_local: bool` — default true
- `routing: RoutingConfig` — blast threshold, tier preferences per task type, cost budget

New env vars: `OPENROUTER_API_KEY`, `LOCAL_MODEL_URL`, `LOCAL_MODEL_NAME`

Config JSON expands with optional `providers` section:
```json
{
  "apiKey": "sk-ds-...",
  "openrouterKey": "sk-or-...",
  "local": [{"name": "ollama", "url": "http://localhost:11434", "model": "qwen3:8b"}],
  "routing": {
    "blastRadiusThreshold": 0.5,
    "preferLocal": true,
    "minTierForPlanning": "premium",
    "minTierForDebugging": "mid"
  }
}
```

New CLI flags: `--providers` (list all with health status), `--set-key openrouter sk-or-xxx`

---

## Phase 3: Auto-Discovery (`src/discovery.rs` — new file)

On startup + every 60s, probe known ports (500ms timeout):

| Server | Port | Health Check | Model Query |
|--------|------|-------------|-------------|
| Ollama | 11434 | `GET /api/tags` | Returns model list with sizes |
| LM Studio | 1234 | `GET /v1/models` | OpenAI format |
| llama.cpp | 8080 | `GET /health` | `/v1/models` |
| vLLM | 8000 | `GET /health` | `/v1/models` |
| Jan | 1337 | `GET /v1/models` | OpenAI format |
| text-gen-webui | 5000 | `GET /v1/models` | OpenAI format |
| KoboldCpp | 5001 | `GET /api/v1/model` | Own format |

Auto-register discovered servers in the registry as `Tier::Free`. Query context window from Ollama's `/api/show` endpoint. Background task maintains health state.

---

## Phase 4: Request/Response Format Transform (`src/transform.rs` — new file)

**This is the hardest part.** Claude Code speaks Anthropic format. All non-Claude providers speak OpenAI format.

### `anthropic_to_openai(body: &Value, model: &str) -> Value`
- `system` field -> prepend as `{"role":"system","content":"..."}` message
- Text content blocks -> plain string content
- `tool_use` blocks -> OpenAI `tool_calls` array with `function` objects
- `tool_result` blocks -> `{"role":"tool","tool_call_id":"...","content":"..."}` messages
- Strip `thinking`, `budget_tokens`, `anthropic-version`, `anthropic-beta`
- Set `stream: true`, target `model`

### `openai_sse_to_anthropic_sse(chunk: &str) -> Option<String>`
Real-time streaming conversion:
- First content chunk -> emit `message_start` + `content_block_start`
- `{"choices":[{"delta":{"content":"X"}}]}` -> `content_block_delta` with `text_delta`
- Tool call chunks -> `content_block_delta` with `input_json_delta`
- `finish_reason: "stop"` -> `content_block_stop` + `message_delta` with usage
- `data: [DONE]` -> `message_stop`

**Risk mitigation**: Build comprehensive tests with captured real SSE from both formats. Test with actual Claude Code sessions.

---

## Phase 5: Unified Forwarding (`src/forward.rs`)

### New function: `openai_compat()`
Replaces `deepseek()` and handles ALL non-Claude providers (DeepSeek via OpenAI endpoint, OpenRouter, Ollama, LM Studio, etc.):
- Takes `base_url`, `api_key`, `model`, `provider_id` from RouteDecision
- Calls `transform::anthropic_to_openai()` on request body
- Streams response through `transform::openai_sse_to_anthropic_sse()`
- Same DB logging / cost tracking as existing functions
- OpenRouter-specific headers: `HTTP-Referer`, `X-Title`

### Generic fallback chain
Replace hardcoded Claude->DeepSeek fallback with `forward_with_fallback()`:
- On connection failure or 5xx: mark provider Down, pick next from registry fallback chain
- Re-transform body if switching between ProviderKinds
- Re-run `context::ensure_fits()` with new provider's context window

---

## Phase 6: Intelligent Multi-Factor Routing (`src/router.rs` — full rewrite)

### Task Classification
```
TaskType: Planning | Debugging | ToolResult | SimpleEdit | FileRead | CodeGeneration | Default
```
Determined by analyzing the request body:
- Last message is all tool_results -> `ToolResult`
- 3+ recent errors -> `Debugging`
- <= 2 messages, no tools -> `Planning`
- User text contains "plan"/"architect"/"design"/"refactor" -> `Planning`
- User text contains "fix"/"error"/"bug"/"debug" -> `Debugging`
- Has tool history -> `CodeGeneration`

### Routing Signals (gathered in main.rs before routing)
```
RoutingSignals {
    blast_radius_score: Option<f64>,   // 0.0-1.0 from deps.rs
    task_type: TaskType,
    recent_error_count: usize,
    message_count: usize,
    has_tool_history: bool,
    is_tool_result_turn: bool,
    estimated_tokens: usize,
    requires_tool_support: bool,       // request has tool definitions
    hourly_spend: f64,
}
```

### Multi-Factor Routing Algorithm

**Step 1 — Determine minimum required tier:**

| Signal | Min Tier |
|--------|----------|
| Planning task | Premium |
| Debugging task | Mid |
| 3+ recent errors | Premium |
| blast_radius > 0.7 | Premium |
| blast_radius 0.4-0.7 | Mid |
| blast_radius < 0.4 | Free |
| ToolResult turn | Free |
| FileRead turn | Free |
| SimpleEdit | Budget |
| CodeGeneration | Mid |
| Default | Mid |

Take the **maximum** of all applicable signals.

**Step 2 — Context window constraint:** Filter out providers whose context window < estimated_tokens. This may force a higher tier.

**Step 3 — Tool support constraint:** If request has tool_use blocks, filter out providers with `supports_tools == false`.

**Step 4 — Select provider:** From registry, pick cheapest healthy provider at the required tier. If none available, walk UP tiers until one is found.

**Step 5 — Return full RouteDecision** with provider_id, kind, tier, model, base_url, api_key, context_window, reason.

---

## Phase 7: Context Window Management (`src/context.rs`)

Replace hardcoded 60K/190K limits with provider-aware thresholds:
- `prepare()` and `ensure_fits()` take `context_window: usize` parameter
- Thresholds become relative: <85% window = fine, 85-95% = compress, 95-100% = trim, >100% = emergency
- Local models with 4K-8K windows get aggressive trimming
- Router already avoids sending large contexts to small-window providers (Phase 6, Step 2)

---

## Phase 8: Cost, Stats, Dashboard, DB

**`src/cost.rs`**: Look up pricing from ProviderConfig instead of hardcoded table. Local models = $0.00. OpenRouter prices queried from `/v1/models` at startup.

**`src/state.rs`**: Replace fixed `claude_turns`/`deepseek_turns` with `HashMap<String, u64>` for provider and tier counts.

**`src/db.rs`**: Migration v5 — add `tier TEXT` column to requests. Update `get_stats()` to use `GROUP BY provider` instead of hardcoded WHERE clauses.

**`src/dashboard.rs`**: Dynamic provider cards (one per provider with non-zero turns). Provider health status section with green/yellow/red dots. Tier distribution chart. "Local savings" metric.

---

## Implementation Order

| Step | What | Test Checkpoint |
|------|------|----------------|
| 1 | `provider.rs` types + registry, wire into ProxyState | Compiles, existing routing unchanged |
| 2 | Expand `config.rs` with new fields | Existing configs still load, new env vars work |
| 3 | `transform.rs` + `forward::openai_compat()`, migrate DeepSeek to it | DeepSeek still works via OpenAI-compat path |
| 4 | `discovery.rs` + local model registration | `--providers` shows discovered Ollama |
| 5 | OpenRouter provider support | OpenRouter models work end-to-end |
| 6 | Rewrite `router.rs` with multi-factor scoring | Routing decisions are demonstrably smarter |
| 7 | Generic fallback chains + health tracking | Graceful degradation when providers go down |
| 8 | Dashboard, cost, DB, stats updates | Dashboard shows all providers dynamically |

---

## Key Risk: SSE Format Conversion

The `transform.rs` response conversion (OpenAI SSE -> Anthropic SSE) is the highest-risk component. Claude Code expects a precise Anthropic event sequence. Must test with:
1. Simple text responses
2. Tool use responses (function calls)
3. Streaming with multiple content blocks
4. Error responses
5. Long responses with many chunks

Build unit tests with captured real SSE payloads from both formats before going live.

---

## Files Modified
- `src/router.rs` — full rewrite
- `src/forward.rs` — add `openai_compat()`, generic fallback
- `src/config.rs` — expand with multi-provider config
- `src/main.rs` — signal gathering, provider registry init, health checks, transform step
- `src/context.rs` — parameterize on context_window
- `src/cost.rs` — dynamic pricing lookup
- `src/state.rs` — dynamic provider counters, registry in state
- `src/db.rs` — migration v5, dynamic stats queries
- `src/dashboard.rs` — dynamic provider cards

## Files Created
- `src/provider.rs` — ProviderConfig, ProviderKind, Tier, ProviderRegistry
- `src/transform.rs` — Anthropic <-> OpenAI bidirectional conversion
- `src/discovery.rs` — local server auto-discovery

## Verification
1. `cargo test` — all existing tests pass + new transform/routing tests
2. Start with `--direct` — Claude-only mode still works
3. Start with DeepSeek key — routing between Claude/DeepSeek works as before
4. Start with Ollama running — auto-discovers, routes tool_result turns to local
5. Start with OpenRouter key — can route to OpenRouter models
6. `--providers` flag — shows all configured/discovered providers with health
7. Dashboard — shows dynamic provider cards, tier distribution
8. Kill Ollama mid-session — graceful fallback to cloud provider
