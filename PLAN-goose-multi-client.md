# Plan: Multi-Client Support (Goose + Claude Code) & Rate-Limit Awareness

## Vision

AISmush becomes a **client-agnostic intelligence layer** — any AI coding agent (Claude Code, Goose, future agents) connects to AISmush and gets compression, file caching, memory, smart routing, and rate-limit protection. The proxy handles both Anthropic and OpenAI API formats in both directions.

---

## Current State

- All incoming requests assumed to be **Anthropic format** (from Claude Code)
- Intelligence layer (file cache, compression, memory, observations) hardcoded to Anthropic message schema
- **21+ hardcoded tool name references** across memory.rs, file_cache.rs, capture.rs, compress.rs
- **15+ Anthropic block type checks** (`tool_use`, `tool_result`, `text`) scattered across 5 files
- Outbound OpenAI conversion exists and works (`transform.rs`)
- No rate-limit awareness — 429s passed through blindly
- No client identification — everything assumed to be Claude Code

## Architecture: Approach C (Normalize Early, Smart)

```
Claude Code ──(Anthropic)──┐                          ┌──(Anthropic)──> Claude API
                           ├──> Normalize ──> Intelligence ──> Route ──┤
Goose (Anthropic) ─────────┤    to Anthropic   Pipeline       Decision ├──(convert)──> OpenAI-compat
                           │    internal fmt                           │
Goose (OpenAI) ──(OpenAI)──┘                          └──(Anthropic)──> DeepSeek /anthropic
                 ↑ convert on entry                    
                                                       
Response path reverses: if client sent OpenAI, convert Anthropic SSE back to OpenAI SSE
```

**Why Approach C**: The intelligence pipeline (file cache, compression, memory, context management) has ~50 touchpoints that assume Anthropic format. Normalizing inbound to Anthropic means zero changes to the core pipeline. The outbound Anthropic→OpenAI transform already exists and is tested. The cost of double-conversion for OpenAI→Anthropic→OpenAI is negligible (~1ms) vs API latency (~2-30s).

---

## Phase 1: Rate-Limit Awareness

**Why first**: This is a real bug affecting users today, and the infrastructure (per-provider health tracking) benefits all later phases.

### 1A: Track rate-limit state per provider

**File: `src/provider.rs`**

Add to `HealthState`:
```rust
pub enum HealthState {
    Healthy,
    Degraded { error_count: usize, last_error: Instant },
    RateLimited { until: Instant, retry_after: Duration },  // NEW
    Down { since: Instant, last_check: Instant },
}
```

**File: `src/forward.rs`** — all three forwarding functions

After receiving upstream response, before streaming:
```rust
if status == 429 {
    // Parse retry-after header (seconds or HTTP-date)
    let retry_after = resp.headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(10);  // default 10s if missing
    
    // Mark provider as rate-limited
    provider_config.health.write().await = HealthState::RateLimited {
        until: Instant::now() + Duration::from_secs(retry_after),
        retry_after: Duration::from_secs(retry_after),
    };
    
    // Log it
    warn!(provider = %provider_id, retry_after_secs = retry_after, "Rate limited by upstream");
}
```

### 1B: Router respects rate-limit state

**File: `src/router.rs`** — `decide_with_registry()`

`is_healthy()` already returns false for `Down`. Extend to also return false for `RateLimited` when `Instant::now() < until`. When the deadline passes, auto-transition back to `Healthy`.

In multi-provider mode: the router naturally skips the rate-limited provider and picks the next best. No special logic needed — just the health check change.

### 1C: Direct mode (single provider) handling

**File: `src/forward.rs`** — `claude()` function

When `--direct` and we get a 429:
1. **Short wait (retry-after <= 5s)**: Hold the connection, sleep, retry once. The user barely notices. Log: "Rate limited, waiting {n}s before retry"
2. **Long wait (retry-after > 5s)**: Pass the 429 through to the client (Claude Code/Goose handles its own retry). But **skip memory injection on the next request** to reduce token burn. Log: "Rate limited for {n}s, reducing token overhead"

### 1D: Adaptive memory injection

**File: `src/main.rs`** — memory injection section (lines 571-576)

Add a `rate_limit_pressure` flag to ProxyState:
```rust
// Skip memory injection if we've been rate-limited recently (< 60s ago)
let under_pressure = state.rate_limit_pressure.load(Ordering::Relaxed);
if !under_pressure {
    if let (Some(ref db), Some(ref mut body_val)) = (&state.db, &mut parsed) {
        memory::inject_memories(db, body_val, &project_path).await;
    }
}
```

Set the flag when a 429 is received, clear it after 60s. This reduces token usage when the key's budget is tight.

### 1E: Dashboard visibility

**File: `src/dashboard.rs`**

Add rate-limit events to the dashboard:
- "Rate limited 3 times in last hour" warning badge
- Per-provider rate-limit history
- Suggestion to add fallback providers if in `--direct` mode

### Phase 1 Files Modified
- `src/provider.rs` — add `RateLimited` variant, update `is_healthy()`
- `src/forward.rs` — parse 429 + retry-after in all three functions, short-wait retry in `claude()`
- `src/state.rs` — add `rate_limit_pressure: AtomicBool`
- `src/main.rs` — conditional memory injection
- `src/router.rs` — no changes needed (health check already gates selection)
- `src/dashboard.rs` — rate-limit stats display

### Phase 1 Test Checkpoints
- [ ] 429 from Claude in `--direct` mode with retry-after <= 5s → proxy holds, retries, succeeds
- [ ] 429 from Claude in `--direct` mode with retry-after > 5s → passed through cleanly
- [ ] 429 from DeepSeek in multi-provider mode → router picks Claude instead
- [ ] Memory injection skipped for 60s after 429
- [ ] Dashboard shows rate-limit events
- [ ] `cargo test` passes

---

## Phase 2: Client Detection & Identification

**Why second**: Needed before tool mapping — we need to know *who* is talking to us.

### 2A: Detect client from request

**File: `src/client.rs`** — new file

```rust
pub enum ClientKind {
    ClaudeCode,
    Goose,
    Unknown(String),  // stores User-Agent for logging
}

pub enum InboundFormat {
    Anthropic,   // messages with content blocks, tool_use/tool_result
    OpenAI,      // messages with role/content strings, tool_calls/function_call
}

pub struct ClientInfo {
    pub kind: ClientKind,
    pub inbound_format: InboundFormat,
}
```

**Detection logic** (in order of reliability):

1. **Request format** — OpenAI format has `messages[].tool_calls` arrays or `functions` field; Anthropic has `messages[].content` as array of typed blocks. Check the structure of the first assistant message with tools.

2. **User-Agent header** — Claude Code sends a distinctive UA. Goose likely sends one too. Fall back to `Unknown`.

3. **Path hint** — If request hits `/v1/chat/completions`, it's OpenAI format. If `/v1/messages`, it's Anthropic format.

### 2B: Wire ClientInfo through the pipeline

**File: `src/main.rs`** — `handle()` function

Detect client immediately after body parsing (line ~493):
```rust
let client_info = client::detect(&parts.headers, &parsed, &path);
```

Pass `client_info` to forwarding functions so response can be converted back to the client's format.

### 2C: Tag conversations in DB

**File: `src/capture.rs`** / **`src/db.rs`**

Add `client TEXT` column to turns table. Migration v6.
```sql
ALTER TABLE turns ADD COLUMN client TEXT DEFAULT 'claude-code';
```

### Phase 2 Files Created
- `src/client.rs` — ClientKind, InboundFormat, detection logic

### Phase 2 Files Modified  
- `src/main.rs` — detect client early, pass through pipeline
- `src/capture.rs` — accept client parameter in store_turn
- `src/db.rs` — migration v6, client column

### Phase 2 Test Checkpoints
- [ ] Anthropic-format request detected as `InboundFormat::Anthropic`
- [ ] OpenAI-format request detected as `InboundFormat::OpenAI`
- [ ] Claude Code User-Agent → `ClientKind::ClaudeCode`
- [ ] Unknown User-Agent → `ClientKind::Unknown`
- [ ] Client column populated in DB

---

## Phase 3: Inbound Format Normalization

**Why third**: With client detection in place, we can now normalize OpenAI-format requests.

### 3A: OpenAI → Anthropic inbound conversion

**File: `src/transform.rs`** — new function

```rust
pub fn openai_to_anthropic(body: &Value) -> Value
```

Reverse of existing `anthropic_to_openai()`:
- `{"role": "system", "content": "..."}` → Anthropic `system` field (string or array)
- `{"role": "user", "content": "text"}` → `{"role": "user", "content": [{"type": "text", "text": "..."}]}`
- `{"role": "assistant", "tool_calls": [...]}` → `{"role": "assistant", "content": [{"type": "tool_use", "id": ..., "name": ..., "input": ...}]}`
- `{"role": "tool", "tool_call_id": "...", "content": "..."}` → merge into preceding user message as `{"type": "tool_result", "tool_use_id": "...", "content": "..."}`
- OpenAI `functions`/`tools` → Anthropic `tools` array with `input_schema`

**Critical edge case**: OpenAI tool results are separate messages with `role: "tool"`. Anthropic tool results are blocks inside `role: "user"` messages. The conversion must group consecutive `tool` messages into a single user message with multiple `tool_result` blocks.

### 3B: Anthropic SSE → OpenAI SSE conversion (response path)

**File: `src/transform.rs`** — new struct

```rust
pub struct AnthropicToOpenAIStream { ... }
```

Reverse of existing `OpenAIToAnthropicStream`:
- `message_start` → first OpenAI chunk with `role: "assistant"`
- `content_block_delta` (text) → `choices[0].delta.content`
- `content_block_start` (tool_use) → `choices[0].delta.tool_calls[i]` with function name
- `content_block_delta` (input_json) → `choices[0].delta.tool_calls[i].function.arguments`
- `message_stop` → `data: [DONE]`

### 3C: Wire normalization into request pipeline

**File: `src/main.rs`** — after client detection

```rust
// Normalize inbound format to Anthropic (our internal representation)
let (mut parsed, body_modified) = if client_info.inbound_format == InboundFormat::OpenAI {
    let anthropic_body = transform::openai_to_anthropic(&parsed.unwrap());
    (Some(anthropic_body), true)
} else {
    (parsed, false)
};

// ... rest of pipeline unchanged (file cache, compression, memory, routing) ...
```

### 3D: Wire format conversion into response path

**File: `src/forward.rs`** — all three functions

If `client_info.inbound_format == InboundFormat::OpenAI`, wrap the response stream:
- For `claude()` and `deepseek()`: pipe Anthropic SSE through `AnthropicToOpenAIStream` before sending to client
- For `openai_compat()`: if client sent OpenAI format, skip the OpenAI→Anthropic SSE conversion and pass the upstream OpenAI SSE directly (optimization: avoid double conversion)

### Phase 3 Files Modified
- `src/transform.rs` — `openai_to_anthropic()`, `AnthropicToOpenAIStream`
- `src/main.rs` — normalize after detection
- `src/forward.rs` — conditional response format conversion

### Phase 3 Test Checkpoints
- [ ] OpenAI-format request with tool_calls → correct Anthropic tool_use blocks
- [ ] OpenAI tool role messages → grouped into Anthropic tool_result blocks
- [ ] Anthropic SSE → OpenAI SSE conversion produces valid stream
- [ ] Full round-trip: OpenAI in → normalize → route to Claude → Anthropic SSE → convert → OpenAI SSE out
- [ ] Shortcut: OpenAI in → route to OpenAI-compat → OpenAI SSE passed through directly
- [ ] `cargo test` passes

---

## Phase 4: Tool Behavior Detection

**Why fourth**: With both formats normalized to Anthropic internally, tool detection works at one layer.

### 4A: Tool classification trait

**File: `src/tools.rs`** — new file

```rust
pub enum ToolCategory {
    FileRead,
    FileWrite,
    Shell,
    Search,
    Other(String),
}

pub struct ToolClassifier {
    /// Configurable name mappings (from config)
    name_map: HashMap<String, ToolCategory>,
}

impl ToolClassifier {
    /// Smart defaults + config overrides
    pub fn new(config_mappings: Option<&ToolMappings>) -> Self { ... }
    
    /// Classify a tool by name first, then by input patterns
    pub fn classify(&self, tool_name: &str, tool_input: Option<&Value>) -> ToolCategory {
        // 1. Exact name match from config
        if let Some(cat) = self.name_map.get(tool_name) {
            return cat.clone();
        }
        
        // 2. Smart defaults — common tool names across agents
        match tool_name.to_lowercase().as_str() {
            // File reading
            s if s.contains("read") || s == "cat" || s == "view" 
              || s == "get_file" || s == "file_content" => FileRead,
            // File writing  
            s if s.contains("write") || s.contains("edit") || s == "patch"
              || s.contains("create_file") || s.contains("replace") => FileWrite,
            // Shell
            s if s.contains("bash") || s.contains("shell") || s.contains("exec")
              || s.contains("command") || s.contains("terminal") || s == "run" => Shell,
            // Search
            s if s.contains("grep") || s.contains("search") || s.contains("find")
              || s.contains("glob") || s.contains("ripgrep") || s == "rg" => Search,
            _ => {}
        }
        
        // 3. Pattern detection from input fields
        if let Some(input) = tool_input {
            if input.get("file_path").is_some() || input.get("path").is_some()
               || input.get("filename").is_some() {
                // Has a file path — likely file operation
                if input.get("content").is_some() || input.get("new_string").is_some() {
                    return FileWrite;
                }
                return FileRead;
            }
            if input.get("command").is_some() || input.get("cmd").is_some() {
                return Shell;
            }
            if input.get("pattern").is_some() || input.get("query").is_some()
               || input.get("regex").is_some() {
                return Search;
            }
        }
        
        Other(tool_name.to_string())
    }
    
    /// Extract file path from tool input (regardless of field name)
    pub fn extract_file_path(&self, input: &Value) -> Option<String> {
        input.get("file_path")
            .or_else(|| input.get("path"))
            .or_else(|| input.get("filename"))
            .or_else(|| input.get("file"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    }
    
    /// Extract command from tool input
    pub fn extract_command(&self, input: &Value) -> Option<String> {
        input.get("command")
            .or_else(|| input.get("cmd"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    }
    
    /// Extract search pattern from tool input
    pub fn extract_pattern(&self, input: &Value) -> Option<String> {
        input.get("pattern")
            .or_else(|| input.get("query"))
            .or_else(|| input.get("regex"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    }
}
```

### 4B: Configuration for tool mappings

**File: `src/config.rs`**

New config section:
```json
{
  "toolMappings": {
    "fileRead": ["Read", "read_file", "cat", "view_file", "get_file_contents"],
    "fileWrite": ["Edit", "Write", "write_file", "create_file", "patch_file"],
    "shell": ["Bash", "execute_command", "shell", "run_command"],
    "search": ["Grep", "Glob", "search", "find_files", "ripgrep"]
  }
}
```

Environment variable override: `AISMUSH_TOOL_MAP_FILE_READ="Read,read_file,cat"` etc.

### 4C: Replace hardcoded tool names across codebase

**Files to update:**

| File | Current | Replace with |
|------|---------|-------------|
| `src/memory.rs:33-66` | `match tool_name { "Read" \| "read" => ...` | `match classifier.classify(tool_name, input) { FileRead => ...` |
| `src/file_cache.rs:136` | `name != "Read" && name != "read_file" && ...` | `classifier.classify(name, input) == FileRead` |
| `src/capture.rs:90-119` | `match tool_name { "Read" \| "read" => ...` | `match classifier.classify(tool_name, input) { FileRead => ...` |
| `src/memory.rs:61-66` | Category matching by tool name | `classifier.classify()` result directly |

For field extraction (`file_path`, `command`, `pattern`):

| File | Current | Replace with |
|------|---------|-------------|
| `src/memory.rs:34-55` | `input.get("file_path")` | `classifier.extract_file_path(input)` |
| `src/file_cache.rs:142` | `i.get("file_path").or_else(\|\| i.get("path"))` | `classifier.extract_file_path(i)` |
| `src/capture.rs:94-118` | Various `.get("file_path")`, `.get("command")`, `.get("pattern")` | `classifier.extract_file_path()`, `extract_command()`, `extract_pattern()` |

### 4D: Wire classifier into state

**File: `src/state.rs`**

Add `tool_classifier: ToolClassifier` to `ProxyState`. Initialize from config at startup.

### Phase 4 Files Created
- `src/tools.rs` — ToolCategory, ToolClassifier with pattern detection

### Phase 4 Files Modified
- `src/config.rs` — ToolMappings config section
- `src/state.rs` — add ToolClassifier to ProxyState
- `src/memory.rs` — use classifier instead of hardcoded names
- `src/file_cache.rs` — use classifier instead of hardcoded names
- `src/capture.rs` — use classifier instead of hardcoded names

### Phase 4 Test Checkpoints
- [ ] Claude Code `Read` tool → classified as FileRead
- [ ] Goose `read_file` tool → classified as FileRead  
- [ ] Unknown tool with `file_path` input → classified as FileRead (pattern detection)
- [ ] Unknown tool with `command` input → classified as Shell
- [ ] Config overrides respected: custom tool name maps to correct category
- [ ] File cache works with Goose tool names
- [ ] Observation extraction works with Goose tool names
- [ ] `cargo test` passes

---

## Phase 5: Dashboard & Observability

### 5A: Client-aware dashboard

**File: `src/dashboard.rs`**

- Show requests by client (Claude Code vs Goose vs Unknown)
- Provider health with rate-limit status (green/yellow/red)
- Rate-limit event timeline
- Token overhead from memory injection (visible so users can see the cost)

### 5B: Stats by client

**File: `src/state.rs`** / **`src/db.rs`**

- `client_turns: HashMap<String, u64>` in Stats
- DB queries: `GROUP BY client` alongside existing `GROUP BY provider`

### 5C: Setup wizard for Goose

**File: `src/setup.rs`**

Add Goose configuration to `--setup`:
- Detect if Goose is installed (`which goose` or check `~/.config/goose/`)
- Show how to point Goose at AISmush: `ANTHROPIC_HOST=http://127.0.0.1:{port}`
- Offer to write Goose config automatically

### Phase 5 Files Modified
- `src/dashboard.rs` — client breakdown, rate-limit display
- `src/state.rs` — per-client stats
- `src/db.rs` — per-client queries, migration
- `src/setup.rs` — Goose setup flow

---

## Implementation Order

| Step | Phase | What | Risk | Test |
|------|-------|------|------|------|
| 1 | 1A-1B | Rate-limit tracking + router awareness | Low | Multi-provider skips rate-limited provider |
| 2 | 1C | Direct mode: short-wait retry | Low | 429 with retry-after=3 → transparent retry |
| 3 | 1D | Adaptive memory injection | Low | Memory skipped after 429 |
| 4 | 2A-2B | Client detection | Low | UA detection, format detection |
| 5 | 4A-4D | Tool classifier | Medium | All existing tests still pass with classifier |
| 6 | 3A | Inbound OpenAI→Anthropic conversion | High | Full format round-trip |
| 7 | 3B | Outbound Anthropic→OpenAI SSE conversion | High | Streaming fidelity |
| 8 | 3C-3D | Wire normalization into pipeline | High | End-to-end Goose flow |
| 9 | 2C, 5 | Dashboard, DB, setup | Low | Visual verification |

**Note**: Steps 1-5 can each be shipped independently. Steps 6-8 are the high-risk format conversion work and should be done together as a unit with extensive testing.

---

## Risk Analysis

### High Risk: SSE Format Round-Trip (Phase 3)

The Anthropic→OpenAI SSE converter (`AnthropicToOpenAIStream`) is new and must produce pixel-perfect output. Goose will reject malformed SSE. Must test with:
1. Simple text responses
2. Multi-tool-call responses
3. Streaming with thinking blocks (Goose probably ignores these)
4. Error responses (4xx, 5xx)
5. Very long responses (100K+ tokens)
6. Interrupted streams (client disconnect)

**Mitigation**: Capture real Goose<->Claude sessions as test fixtures before building the converter.

### Medium Risk: Tool Pattern Detection (Phase 4)

Pattern-based detection could misclassify tools. Example: a tool named `read_config` with a `path` input could be classified as FileRead when it's actually reading a config key, not a file.

**Mitigation**: Exact name matches take priority. Pattern detection is the fallback. Config overrides let users fix misclassifications. Log classifications so issues are visible.

### Low Risk: Rate-Limit Handling (Phase 1)

Well-understood problem. The retry-after header is standardized. The health state infrastructure already exists.

---

## OpenAI Inbound: Endpoint Consideration

Goose configured with `OPENAI_HOST=http://127.0.0.1:{port}` will send requests to `/v1/chat/completions`. Currently the proxy only handles Anthropic paths (`/v1/messages`).

**Solution** (in Phase 3C): Add path routing in `main.rs`:
```rust
if path.starts_with("/v1/chat/completions") {
    // OpenAI inbound — normalize to Anthropic, then standard pipeline
} else if path.starts_with("/v1/messages") {
    // Anthropic inbound — standard pipeline  
} else {
    // Existing dashboard/API/static handlers
}
```

This also means Goose users can connect via EITHER:
- `ANTHROPIC_HOST=http://127.0.0.1:1849` (Anthropic format, minimal conversion)
- `OPENAI_HOST=http://127.0.0.1:1849` (OpenAI format, full conversion)

Recommend Anthropic mode when Goose is using Claude as its provider (zero conversion overhead), and OpenAI mode for all other providers.

---

## Files Summary

### New Files
- `src/client.rs` — Client detection (kind + format)
- `src/tools.rs` — Tool classification (pattern + config)

### Modified Files
| File | Phase | Changes |
|------|-------|---------|
| `src/provider.rs` | 1 | RateLimited health state |
| `src/forward.rs` | 1, 3 | 429 handling, response format conversion |
| `src/state.rs` | 1, 4, 5 | rate_limit_pressure, ToolClassifier, per-client stats |
| `src/main.rs` | 1, 2, 3 | Adaptive memory, client detection, format normalization, path routing |
| `src/router.rs` | 1 | Health check respects RateLimited (minimal) |
| `src/transform.rs` | 3 | openai_to_anthropic(), AnthropicToOpenAIStream |
| `src/config.rs` | 4 | ToolMappings config section |
| `src/memory.rs` | 4 | Use ToolClassifier |
| `src/file_cache.rs` | 4 | Use ToolClassifier |
| `src/capture.rs` | 4 | Use ToolClassifier, client column |
| `src/compress.rs` | 4 | Use ToolClassifier (minor) |
| `src/dashboard.rs` | 5 | Client breakdown, rate-limit display |
| `src/db.rs` | 5 | Migration v6, per-client queries |
| `src/setup.rs` | 5 | Goose setup flow |

---

## Verification: End-to-End Scenarios

1. **Claude Code → Claude API (--direct)**: Unchanged behavior. Rate-limit awareness added.
2. **Claude Code → Claude API (multi-provider)**: 429 from Claude → route to DeepSeek. Rate limit clears → route back.
3. **Goose (Anthropic mode) → Claude API**: Goose sets `ANTHROPIC_HOST`. File cache, compression, memory all work with tool classifier.
4. **Goose (OpenAI mode) → Claude API**: Goose sets `OPENAI_HOST`. Request normalized to Anthropic, routed to Claude, response converted back to OpenAI SSE.
5. **Goose (OpenAI mode) → Ollama**: Request normalized, intelligence applied, converted to OpenAI for Ollama, response converted back to OpenAI SSE for Goose.
6. **Mixed: Claude Code + Goose on same proxy**: Both connect, both get intelligence features, dashboard shows both. Shared rate-limit budget managed by proxy.
