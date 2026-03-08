# Implementation Plan: Codebase Improvements + Server Mode

**Date**: 2026-03-07
**Branch**: `claude/explore-codebase-improvements-TOhIe`
**Scope**: Tier 1 + Tier 2 improvements from competitive analysis, plus HTTP server mode

---

## Phase 1: Context Compaction [CRITICAL PATH]

The single biggest gap. Without this, long sessions hit token limits and fail.

### 1A. Token Estimation Layer

**File**: `crates/rho-core/src/compaction.rs`

Current state: `estimate_tokens()` exists using chars/4 heuristic. This is adequate as a starting point — actual tokenizer (tiktoken-rs) can be swapped in later without changing the interface.

**Changes**:
- Add `estimate_message_tokens(msg: &Message) -> u64` that handles all Content variants (Text, Thinking, ToolCall, Image)
- Add `estimate_conversation_tokens(msgs: &[Message]) -> u64` for total count
- Wire `Model.context_window` (currently stored but never read) into the compaction decision

### 1B. Tool Output Pruning

**File**: `crates/rho-core/src/compaction.rs`

Current state: `prune_tool_outputs()` exists but is basic. Improve it:

**Changes**:
- Walk backwards from most recent message
- Protect the last ~40K tokens of tool output (configurable via `compact_protect_tokens` in RHO.md)
- Replace older ToolResult content with `"[output truncated — {N} tokens removed]"`
- Preserve tool names and error status (just truncate content)

### 1C. Auto-Compaction Transform

**File**: `crates/rho-core/src/compaction.rs` + `crates/rho-core/src/agent_loop.rs`

**Changes**:
- Implement the `transform_messages` hook that's already wired into agent_loop
- Two-phase strategy:
  1. **Phase 1** (at 80% window): Prune old tool outputs
  2. **Phase 2** (at 90% window): Summarize old conversation turns
    - Take messages[0..len-N] where N = recent protected messages
    - Build a summary prompt: "Summarize the key context, decisions, files modified, and current task state"
    - Call the same LLM (or a fast model like Haiku) to produce a summary
    - Replace old messages with a single User message containing the summary
- Emit `ContextCompacted` event with before/after token counts
- Make threshold configurable: `compact_threshold: 0.8` in RHO.md (already parsed in config.rs)

### 1D. Fix `await_holding_lock` Bug

**File**: `crates/rho-core/src/event_stream.rs`

**Changes**:
- Replace `Arc<std::sync::Mutex<...>>` with `Arc<tokio::sync::Mutex<...>>` for the receiver field
- Update `next()` and Stream impl to use `.await` on the tokio mutex
- Add `tokio` dependency to rho-core if not already present (it is — full features)

**Testing**:
- Existing tests in event_stream.rs should still pass
- Add a concurrent access test to verify no deadlock

---

## Phase 2: Tool Reduction & System Prompt Optimization

### 2A. Reduce Default Tools to 5

**File**: `src/main.rs` (tool assembly section, ~lines 310-370)

**Keep**: `read`, `write`, `edit`, `bash`, `task`
**Remove from default set**: `grep`, `find`, `web_fetch`, `web_search`

**Changes**:
- Move grep/find/web_fetch/web_search to an `extended_tools` set
- Default tool list = 5 core tools
- CLI flag `--tools-extended` or `--all-tools` loads the full 9
- RHO.md `allowed_tools` config still works for explicit selection
- Don't delete the tool code — just exclude from default registration

**System prompt update**:
- Add brief guidance: "Use bash for file search (rg, fd), web requests (curl), and web search as needed"
- Remove detailed descriptions of dropped tools from base prompt

### 2B. Lazy-Load Tool Descriptions

**Files**: `crates/rho-core/src/tool.rs`, `src/main.rs` (system prompt assembly)

**Current state**: Full tool JSON schemas are serialized into the system prompt AND sent as tool definitions to the API. The API tool definitions are required, but the system prompt descriptions are redundant.

**Changes**:
- Add `fn brief_description(&self) -> String` to `AgentTool` trait (one-liner, ~10 words)
- System prompt includes only brief descriptions in an `<available_tools>` block
- Full `parameters_schema()` still sent to the API as tool definitions (required by Anthropic API)
- Skills: only include `name` + `description` from frontmatter in system prompt; full SKILL.md body available via a "load skill" mechanism (or just bash `cat`)

**Estimated token savings**: 2-3K tokens from system prompt

### 2C. Adopt AGENTS.md Convention

**File**: `crates/rho-core/src/config.rs`

**Changes**:
- In `load_project_config()`, check for files in this order:
  1. `RHO.md` (rho-native, highest priority)
  2. `AGENTS.md` (ecosystem standard)
  3. `CLAUDE.md` (Claude Code compat, current behavior)
- Parse AGENTS.md with same YAML frontmatter + markdown body format
- Log which config file was loaded: `[config] loaded from AGENTS.md`

---

## Phase 3: Server Mode (HTTP + SSE)

### Architecture

```
┌──────────────────────────────────────────────┐
│  rho serve (HTTP server on localhost:PORT)    │
│                                              │
│  POST /v1/sessions          → create session │
│  POST /v1/sessions/:id/send → send message   │
│  GET  /v1/sessions/:id/events → SSE stream   │
│  GET  /v1/sessions/:id      → session info   │
│  GET  /v1/sessions          → list sessions  │
│  DELETE /v1/sessions/:id    → end session     │
│                                              │
│  GET  /health               → health check   │
└──────────────────────────────────────────────┘
          ↕ SSE events
┌──────────────┐  ┌──────────────┐  ┌──────────────┐
│  CLI client  │  │  Web client  │  │  GUI client  │
│  (rho --     │  │  (browser)   │  │  (Iced)      │
│   connect)   │  │              │  │              │
└──────────────┘  └──────────────┘  └──────────────┘
```

### 3A. New Crate: `rho-server`

**New file**: `crates/rho-server/src/lib.rs`

**Dependencies**: `axum` (async web framework, Tokio-native), `tower`, `tokio`

**Why axum**: Rust-native, async, built on Tokio (already in stack), minimal overhead, great SSE support via `axum::response::Sse`. No macro magic, just functions.

**Workspace addition**: Add `rho-server` to `Cargo.toml` members

### 3B. API Design

```rust
// POST /v1/sessions
// Request:
{
    "model": "claude-sonnet",          // optional, default from config
    "thinking": "medium",              // optional
    "cwd": "/path/to/project",        // optional, default server cwd
    "system_append": "extra context",  // optional
    "tools": ["read", "write", "edit", "bash", "task"]  // optional
}
// Response:
{
    "id": "session_abc123",
    "model": "claude-sonnet-4-6",
    "created_at": 1741305600
}

// POST /v1/sessions/:id/send
// Request:
{
    "content": "add error handling to auth module",
    "type": "user"  // "user" or "follow_up"
}
// Response:
{
    "accepted": true,
    "turn_index": 0
}

// GET /v1/sessions/:id/events (SSE stream)
// Events:
data: {"type": "turn_start", "turn_index": 0}
data: {"type": "text_delta", "delta": "I'll add error..."}
data: {"type": "thinking_delta", "delta": "Let me analyze..."}
data: {"type": "tool_start", "tool": "read", "args": {"path": "src/auth.rs"}}
data: {"type": "tool_end", "tool": "read", "is_error": false}
data: {"type": "turn_end", "turn_index": 0, "usage": {"input": 1234, "output": 567}}
data: {"type": "agent_end"}
```

### 3C. Server Implementation

**File**: `crates/rho-server/src/lib.rs` (~400 lines)

**Core structure**:

```rust
struct ServerState {
    sessions: Arc<RwLock<HashMap<String, SessionHandle>>>,
    config: ProjectConfig,
    session_store: SessionStore,
}

struct SessionHandle {
    id: String,
    messages: Vec<Message>,
    event_tx: broadcast::Sender<ServerEvent>,  // For SSE clients
    cancel: CancellationToken,
    model: ModelConfig,
    cwd: PathBuf,
}
```

**Key design decisions**:
- Each session gets its own `broadcast::Sender` for SSE fan-out (multiple clients can watch same session)
- Agent loop runs in a spawned Tokio task, pushes events to broadcast channel
- SSE endpoint converts `AgentEvent` → JSON `ServerEvent` for the wire
- Session state persisted to SQLite (reuse `rho-session`)
- Server binds to `127.0.0.1` by default (localhost only). `--bind 0.0.0.0` for remote access with a warning

### 3D. CLI Integration

**File**: `src/main.rs`

**New subcommand**: `rho serve`

```
rho serve [OPTIONS]
    --port <PORT>       Port to listen on (default: 7890)
    --bind <ADDR>       Bind address (default: 127.0.0.1)
    --no-auth           Disable bearer token auth (default: require token)
```

**New subcommand**: `rho connect`

```
rho connect [OPTIONS]
    --url <URL>         Server URL (default: http://localhost:7890)
    --session <ID>      Resume existing session
```

This turns the CLI into an SSE client that renders events to terminal, exactly like the current direct mode but over HTTP.

### 3E. Authentication (Simple)

**Approach**: Bearer token, generated on server start, printed to stdout.

```
$ rho serve
[server] listening on http://127.0.0.1:7890
[server] auth token: rho_sk_a1b2c3d4e5f6...
[server] pass --no-auth to disable authentication
```

Clients pass `Authorization: Bearer rho_sk_...` header. Simple, no OAuth complexity.

For remote access: the user is responsible for TLS (e.g., via SSH tunnel or reverse proxy).

---

## Phase 4: Crate Consolidation

### 4A. Merge Plan

| New Structure | Absorbs | Rationale |
|---------------|---------|-----------|
| `rho-core` | + `rho-session` + `rho-hashline` + `rho-lib` | Core types, loop, persistence, hashline are tightly coupled in practice |
| `rho-tools` | (unchanged) | Tool implementations, depends on rho-core |
| `rho-provider` | + `anthropic-auth` | Provider + auth are one concern |
| `rho-server` | (new) | HTTP server mode |
| `rho-gui` | (unchanged) | GUI is legitimately separate |

**Delete**: `rho-lib` (empty stub), `rho-cli` (redundant stub)

**Result**: 9 crates → 5 crates

### 4B. Execution Strategy

- Move `rho-session/src/*` into `rho-core/src/session/`
- Move `rho-hashline/src/*` into `rho-core/src/hashline/`
- Update all `use` imports across the workspace
- Delete empty crate directories
- Update `Cargo.toml` workspace members
- Run `cargo test` to verify

---

## Phase 5: Quality of Life

### 5A. Auto-Commit on Edit (configurable)

**File**: `crates/rho-tools/src/edit.rs`, `crates/rho-tools/src/write.rs`

**Changes**:
- After successful file write/edit, if `auto_commit: true` in config:
  - `git add <file>`
  - `git commit -m "rho: <brief description>"`
- Off by default. Enable in RHO.md: `auto_commit: true`
- Skip if not in a git repo or if file is in .gitignore

### 5B. Session Branching

**File**: `crates/rho-core/src/session/` (after merge)

**Changes**:
- Add `parent_id` column to sessions table
- `rho --branch` creates a new session forked from current state
- Messages from parent session loaded as prefix
- Enables speculative execution: try approach A, if it fails, branch back and try B

### 5C. Planning Mode

**File**: `crates/rho-core/src/agent_loop.rs`

**Changes**:
- New config flag: `planning: true` (or CLI `--plan`)
- When enabled, first turn uses a "produce a step-by-step plan" system prompt suffix
- Plan displayed to user, who can approve/edit/reject
- If approved, subsequent turns execute the plan steps
- Plan stored in session for reference

---

## Execution Order & Dependencies

```
Phase 1 (Week 1-2): Context Compaction + Bug Fix
  ├── 1D. Fix await_holding_lock (30 min, do first)
  ├── 1A. Token estimation (2 hours)
  ├── 1B. Tool output pruning improvements (2 hours)
  └── 1C. Auto-compaction transform (4 hours)

Phase 2 (Week 2): Tool Reduction + Prompt Optimization
  ├── 2A. Reduce to 5 default tools (1 hour)
  ├── 2B. Lazy-load descriptions (2 hours)
  └── 2C. AGENTS.md support (30 min)

Phase 3 (Week 3-4): Server Mode
  ├── 3A. Create rho-server crate (1 hour)
  ├── 3B. API routes + handlers (4 hours)
  ├── 3C. SSE event streaming (3 hours)
  ├── 3D. CLI serve/connect subcommands (2 hours)
  └── 3E. Bearer token auth (1 hour)

Phase 4 (Week 4): Crate Consolidation
  └── 4A-4B. Merge crates 9→5 (3 hours)

Phase 5 (Week 5+): Quality of Life
  ├── 5A. Auto-commit on edit (1 hour)
  ├── 5B. Session branching (3 hours)
  └── 5C. Planning mode (4 hours)
```

**Total estimated effort**: ~30 hours of focused work

---

## Key Design Principles

1. **Don't break what works.** Agent loop, hashline editing, session persistence — these are solid. Build on them.
2. **Server mode reuses everything.** The HTTP server is a thin adapter over the same `agent_loop()` + `EventStream` that CLI and GUI already use.
3. **Compaction is the unlock.** Without it, nothing else matters for real-world usage.
4. **Fewer tools = better model performance.** Trust the model to use bash for secondary operations.
5. **Configuration, not code.** Auto-commit, tool sets, compaction thresholds — all configurable via RHO.md/AGENTS.md.

---

## Risk & Mitigation

| Risk | Mitigation |
|------|------------|
| Compaction loses important context | Protect recent 40K tokens; include summary of what was pruned |
| Removing tools degrades task completion | Keep tools available via `--all-tools`; measure before committing |
| Server mode security (remote access) | Default localhost-only; bearer token required; warn on 0.0.0.0 |
| Crate merge breaks builds | Do it last; comprehensive `cargo test` after each merge step |
| axum adds dependency weight | axum is lightweight (~5K lines); already uses Tokio stack |
