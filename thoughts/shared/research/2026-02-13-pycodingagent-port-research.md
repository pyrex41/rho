---
date: 2026-02-13T12:00:00-08:00
researcher: reuben
git_commit: (initial - no commits yet)
branch: main
repository: rho
topic: "PyCodingAgent Port - Architecture Research and Language Evaluation"
tags: [research, codebase, port, architecture, language-selection, rust, iced, gui, cargo-workspace]
status: complete
last_updated: 2026-02-13
last_updated_by: reuben
last_updated_note: "Final decision: Rust (reversed from OCaml) — GUI/Iced requirement, Cargo workspace structure, crate selections, configurable tool backends"
---

# Research: PyCodingAgent Port - Architecture & Language Evaluation

**Date**: 2026-02-13
**Researcher**: reuben
**Git Commit**: (initial - no commits yet)
**Branch**: main
**Repository**: rho

## Research Question
Understand the PiCodingAgent reference implementation, its architecture, tool system, hashline editing, and auth flow — then evaluate language candidates (Rust, OCaml, Nim, Zig, Common Lisp) for the port.

## Summary

The "PyCodingAgent" (actually **PiCodingAgent**, a TypeScript/Bun codebase by Mario Zechner + Can Bölük's fork) is a minimal coding agent built on a ~400-line core loop with 6 tools, streaming Anthropic API integration, hashline-based file editing, and Claude Code OAuth support. The reference materials in `reference/` are comprehensive and include full source for every component needed.

The core architecture is:
1. **Agent loop** (~400 LOC) — prompt → LLM → tool dispatch → repeat
2. **6 tools** — Read, Edit (hashline), Write, Bash, Grep, Find
3. **Hashline system** (~990 LOC) — line-addressable edits with xxHash32 integrity checks
4. **Anthropic provider** (~850 LOC) — SSE streaming, OAuth/API key auth, Claude Code identity
5. **Event stream** (~90 LOC) — typed async iterator for streaming events

## Detailed Findings

### 1. Core Agent Loop (`reference/upstream/agent-loop.ts`)

The agent loop is the heart of the system. It's ~418 lines with a clean structure:

- **`agentLoop()`** — Entry point. Takes prompts, context, config. Returns an `EventStream<AgentEvent>`.
- **`agentLoopContinue()`** — Resume from existing context (for retries).
- **`runLoop()`** — The actual loop:
  - Outer loop: handles follow-up messages after agent would normally stop
  - Inner loop: processes tool calls and steering messages
  - Each iteration: stream assistant response → execute tool calls → check for steering/follow-up
- **`streamAssistantResponse()`** — Transforms `AgentMessage[]` → `Message[]` at the LLM boundary, calls the streaming API
- **`executeToolCalls()`** — Sequential tool execution with steering interrupt support

Key design decisions:
- Messages are `AgentMessage[]` internally, only converted to `Message[]` at the LLM call boundary via `convertToLlm()`
- `transformContext()` hook for context pruning/injection before each LLM call
- Steering messages can interrupt tool execution mid-run
- Follow-up messages keep the loop alive after it would otherwise exit
- Tool calls are executed **sequentially**, not in parallel

### 2. Agent Class (`reference/upstream/agent.ts`)

The `Agent` class wraps the loop with state management:

- State: `systemPrompt`, `model`, `tools`, `messages`, `isStreaming`, `pendingToolCalls`, `error`
- Event system: `subscribe(fn)` → listeners get `AgentEvent`s
- Message queues: `steer()` for mid-run interrupts, `followUp()` for post-run messages
- Abort support via `AbortController`
- `waitForIdle()` returns a promise that resolves when the current prompt completes

### 3. Hashline System (`reference/fork/hashline.ts`)

This is the most complex component at ~991 lines. The hashline format provides line-addressable file editing with integrity checks.

**Hash computation:**
- Strip all whitespace from the line
- xxHash32 on the result
- `% 256` → 2-char hex string (e.g., `"a3"`)
- Pre-computed lookup table of 256 entries avoids allocation

**Display format:** `LINE:HASH|content` (e.g., `1:a3|function hello() {`)

**Edit operations** (submitted as JSON array):
- `set_line` — Replace one line by `LINE:HASH` anchor
- `replace_lines` — Replace a range by start/end anchors
- `insert_after` — Insert text after an anchor
- `replace` — Fuzzy text match fallback (no anchors)

**Application algorithm (critical ordering):**
1. Parse all edits
2. Pre-validate ALL hash references before any mutation
3. Hash relocation: if hash mismatches at the given line but exists uniquely elsewhere, auto-relocate
4. Deduplicate identical edits
5. Sort bottom-up (highest line number first)
6. Apply edits with heuristic cleanup

**Heuristic cleanup** (7 heuristics for model mistakes):
1. Strip hashline prefixes from `new_text` (model copies `42:a7|content`)
2. Strip diff `+` markers (model uses unified diff format)
3. Boundary echo stripping (model copies context around edits)
4. Indentation restoration (model strips leading whitespace)
5. Wrapped line restoration (model reflows lines)
6. Merge detection (model merges adjacent lines)
7. Confusable hyphen normalization (Unicode dashes → ASCII)

**Mismatch error format** shows `>>>` markers with correct references so the model can retry without re-reading.

### 4. Anthropic Provider (`reference/upstream/anthropic-provider.ts`)

~852 lines handling the Anthropic Messages API:

**Auth branching** (`createClient()`):
- OAuth token detected by `sk-ant-oat` prefix → Bearer auth + Claude Code identity headers
- API key → standard `x-api-key` header
- GitHub Copilot → Bearer auth, selective betas

**Claude Code identity** (for OAuth):
- `anthropic-beta: claude-code-20250219,oauth-2025-04-20`
- `user-agent: claude-cli/2.1.2 (external, cli)`
- `x-app: cli`
- System prompt prepend: "You are Claude Code, Anthropic's official CLI for Claude."
- Tool name normalization: `bash` → `Bash`, `read` → `Read`, etc.

**SSE streaming:**
- Handles `message_start`, `content_block_start`, `content_block_delta`, `content_block_stop`, `message_delta`
- Supports text, thinking, and tool_use content blocks
- Streaming JSON parsing for tool call arguments
- Adaptive thinking for Opus 4.6+ (effort levels: low/medium/high/max)

### 5. OAuth Authentication (`reference/upstream/claude-code-auth/`)

Complete PKCE OAuth flow for Claude Pro/Max subscription:

1. Generate PKCE verifier + SHA-256 challenge
2. Open browser to `claude.ai/oauth/authorize`
3. Exchange code at `console.anthropic.com/v1/oauth/token`
4. Save credentials to `~/.pi/agent/auth.json`
5. Token refresh with file locking (prevents race conditions)

Key constants: CLIENT_ID `9d1c250a-e61b-44d9-88ed-5944d1962f5e`, scopes `org:create_api_key user:profile user:inference`

### 6. Tool Implementations

**Read tool** (`reference/fork/read-tool.ts`, ~1066 lines):
- Streaming line-by-line file reading with byte/line limits
- Hashline formatting when enabled
- Image support (base64), document conversion (via markitdown)
- Directory listing with modification times
- Fuzzy path matching for file-not-found suggestions
- Internal URL routing (agent://, skill://)

**Edit tool** (`reference/fork/edit-tool.ts`, ~743 lines):
- Three modes: replace (fuzzy text match), patch (structured diff), hashline (line-addressed)
- Mode determined dynamically based on current model
- LSP integration for format-on-write and diagnostics
- BOM handling, line ending normalization (CRLF/LF)

**Grep tool** (`reference/fork/grep-tool.ts`, ~480 lines):
- Powered by native grep bindings (ripgrep library)
- Hashline-formatted output for context lines
- Configurable context before/after, case sensitivity, multiline
- Match limit with offset pagination

**Find tool** (`reference/fork/find-tool.ts`, ~541 lines):
- Glob-based file discovery with gitignore awareness
- Sort by modification time
- Pattern parsing: extracts base directory from glob (e.g., `src/app/**/*.tsx`)
- Timeout protection (5s)

**Bash tool** (`reference/fork/bash-tool.ts`, ~344 lines):
- PTY support for interactive commands
- Timeout (1s-3600s), working directory, head/tail output filtering
- Command normalization: strips piped head/tail, 2>&1
- Streaming output via tail buffer
- Bash interceptor for command blocking

**Write tool** (`reference/fork/write-tool.ts` — not in reference but documented):
- Simple file creation/overwrite
- Minimal — just writes content to path

### 7. Event Stream (`reference/upstream/event-stream.ts`)

Generic async-iterable event stream (~88 lines):
- Queue-based: producers `push()`, consumers `for await...of`
- Completion detection via configurable `isComplete` predicate
- `result()` returns a promise for the final value
- `AssistantMessageEventStream` specialization for LLM responses

### 8. Type System (`reference/upstream/ai-types.ts`, `agent-types.ts`)

Key types:
- `Message = UserMessage | AssistantMessage | ToolResultMessage`
- `AgentMessage = Message | CustomMessages` (extensible via declaration merging)
- `AgentTool<TParameters>` extends `Tool` with `execute()` function
- `AgentEvent` — discriminated union of ~10 event types
- `Model<TApi>` — provider/api/cost/context metadata
- `ThinkingLevel` — off/minimal/low/medium/high/xhigh

## Architecture Documentation

### Component Dependencies

```
Agent
  └── AgentLoop
       ├── streamAssistantResponse
       │    ├── transformContext (AgentMessage[] → AgentMessage[])
       │    ├── convertToLlm (AgentMessage[] → Message[])
       │    └── streamSimple → AnthropicProvider (SSE streaming)
       │         └── createClient (OAuth detection, header injection)
       └── executeToolCalls
            ├── Read tool (hashline formatting)
            ├── Edit tool (hashline/replace/patch modes)
            ├── Write tool
            ├── Bash tool (PTY, timeouts)
            ├── Grep tool (hashline output)
            └── Find tool (glob matching)
```

### Data Flow

```
User prompt
  → AgentMessage[]
    → transformContext() (pruning, injection)
      → convertToLlm() → Message[]
        → AnthropicProvider (SSE stream)
          → AssistantMessage (text, thinking, tool calls)
            → Tool execution (sequential)
              → ToolResultMessage[]
                → Loop back to LLM call
```

### Proposed Port Structure (from REFERENCES.md)

The REFERENCES.md suggests this reading order for the port:
1. `agent-loop.ts` + `agent.ts` — Core loop (~400 lines)
2. `hashline.ts` — Hash computation, formatting, edit application
3. `anthropic-provider.ts` — API client, auth branching, SSE
4. `CLAUDE-CODE-AUTH.md` — Full OAuth flow
5. `edit-tool.ts` — Edit schemas and hashline wiring
6. `read-tool.ts` — File formatting with hashline prefixes

### Design Principles (from FAST-TOOLS-AND-HASHLINE.md, adapted)

- **Native by default, configurable backends**: Use Rust crates (ripgrep internals) natively, but allow users to configure external binaries via `tools.toml`
- **Streaming-first**: Large files stream hashlines in chunks
- **Minimal system prompt**: Models know how to code from RL training
- **4-6 tools maximum**: Read, Edit, Write, Bash, Grep, Find
- **Workspace of crates**: `rho-core`, `rho-hashline`, `rho-tools`, `rho-provider`, `anthropic-auth`, `rho-gui`

## Language Evaluation for Port

### Dimensions to Evaluate

| Dimension | What Matters |
|-----------|-------------|
| Startup time | Agent should feel instant — <50ms to first output |
| Binary size | Single static binary, no runtime dependencies |
| Async I/O | SSE streaming, concurrent tool execution, PTY |
| String handling | UTF-8, xxHash32, regex, line manipulation |
| JSON handling | Schema validation, streaming JSON parse |
| HTTP client | SSE/chunked transfer, OAuth PKCE, Bearer auth |
| Process spawning | Bash tool with PTY, timeouts, output capture |
| Regex engine | Ripgrep-quality regex for grep tool |
| File traversal | gitignore-aware parallel directory walking |
| Ecosystem | Crates/packages for the above, not reinventing |

### Rust

**Strengths:**
- `reference/FAST-TOOLS-AND-HASHLINE.md` was written specifically for Rust — crate selections already done
- `ignore` + `globset` + `grep-searcher` = literal ripgrep internals as libraries
- `xxhash-rust`, `memmap2`, `nucleo-matcher` — all production-ready
- `tokio` + `reqwest` + `eventsource-stream` for async HTTP/SSE
- Zero-cost abstractions, tiny binary, no GC
- Best ecosystem for this exact use case (file tools + HTTP streaming)

**Weaknesses:**
- Highest complexity for prototyping — borrow checker, lifetimes
- Async Rust has learning curve (Pin, futures, tower)
- Compile times (incremental helps, but initial build is slow)
- The hashline heuristic cleanup code involves lots of string manipulation — verbose in Rust

**Estimated complexity:** High to implement, but the crate ecosystem does 80% of the work. The agent loop itself is simple; the hashline system is where Rust's verbosity would show.

### OCaml

**Strengths:**
- Excellent pattern matching — hashline edit parsing would be very clean
- Algebraic data types map perfectly to `AgentEvent`, `Message`, etc.
- Fast compilation, decent binary size
- `lwt` or `eio` for async I/O
- Strong type inference reduces boilerplate
- Good string handling (though not as battle-tested for UTF-8 edge cases)

**Weaknesses:**
- HTTP/SSE ecosystem is thin — may need to hand-roll SSE parsing over `cohttp`
- No ripgrep-equivalent library crates — would need to shell out to `rg` or reimplement
- File traversal without gitignore awareness means shelling out or writing it
- Smaller community, fewer battle-tested libraries for this specific domain
- PTY handling would need bindings to C libraries
- JSON schema validation is less ergonomic than Rust's serde

**Estimated complexity:** Medium core implementation, but high for the file tool ecosystem. Would likely need to shell out for grep/find, which contradicts the design principle.

### Nim

**Strengths:**
- Python-like syntax, very fast prototyping
- Compiles to C — small binaries, fast startup
- Good FFI to C libraries
- `asyncdispatch` for async I/O
- Excellent string handling
- Could wrap ripgrep's C API or use PCRE

**Weaknesses:**
- Ecosystem is small — no ripgrep library bindings, no gitignore-aware walker
- HTTP/SSE would need `httpclient` + manual SSE parsing
- Community is tiny — fewer maintained libraries
- Less production-proven for this kind of tool
- Memory management (choice of GC vs ARC) adds decisions
- Would likely need to shell out for grep/find

**Estimated complexity:** Low-medium for the core agent loop, but high for building the file tool ecosystem from scratch.

### Zig

**Strengths:**
- Tiny binaries, instant startup, no runtime
- Excellent C interop — could link ripgrep's C libraries
- `comptime` for compile-time computation (hash lookup tables)
- Manual memory management gives full control
- Cross-compilation is trivial

**Weaknesses:**
- No async runtime — would need to build event loop or use `io_uring`
- Ecosystem is nascent — HTTP client, JSON parsing, SSE all need work
- String handling is byte-level — UTF-8 manipulation is manual
- No regex in stdlib — would need PCRE bindings or zig-regex
- The hashline heuristic cleanup code would be extremely verbose
- JSON schema validation from scratch
- This is essentially "write everything yourself" territory

**Estimated complexity:** Very high. The agent loop is simple, but every dependency (HTTP, SSE, JSON, regex, glob, gitignore) would need wrapping or reimplementing.

### Common Lisp

**Strengths:**
- REPL-driven development — fastest iteration cycle
- Dynamic typing makes prototyping the agent loop trivial
- Macros could elegantly express the hashline edit DSL
- Excellent string manipulation (CL's string library + CL-PPCRE for regex)
- Condition system is perfect for the hashline mismatch error handling
- SBCL produces fast native code
- Could integrate with the existing Cortex system (already a CL image!)

**Weaknesses:**
- HTTP/SSE: `dexador` for HTTP, but SSE parsing needs custom code
- Binary distribution: SBCL images are large (~50MB+) and platform-specific
- No ripgrep-equivalent — would need to shell out or use CL-PPCRE with file walking
- gitignore-aware traversal doesn't exist — would need to implement
- JSON handling via `jonathan` or `yason` works but schema validation is manual
- Community is small for this domain
- PTY handling would need CFFI bindings

**Estimated complexity:** Low-medium for the core loop (dynamic typing is great for prototyping), but the file tool ecosystem would need significant work. The Cortex integration angle is interesting but orthogonal.

### Comparison Matrix

| Factor | Rust | OCaml | Nim | Zig | Common Lisp |
|--------|------|-------|-----|-----|-------------|
| Core loop complexity | Medium | Low | Low | Medium | Low |
| File tool ecosystem | Excellent | Poor | Poor | Poor | Poor |
| HTTP/SSE support | Excellent | Fair | Fair | Poor | Fair |
| Hashline impl | Good | Excellent | Good | Verbose | Good |
| Binary distribution | Excellent | Good | Good | Excellent | Poor |
| Startup time | Excellent | Good | Excellent | Excellent | Poor (image load) |
| Prototyping speed | Slow | Medium | Fast | Slow | Fastest |
| Production readiness | Excellent | Good | Fair | Fair | Good |

## Decision History

### Round 1: Five-Language Evaluation → Rust vs OCaml

Narrowed from Rust, OCaml, Nim, Zig, Common Lisp down to Rust and OCaml as the two serious contenders (see comparison matrix above).

### Round 2: OCaml Initially Chosen (subsequently reversed)

With the constraint that **shelling out is acceptable**, OCaml initially won because:
- Pattern matching is excellent for the hashline heuristic cleanup code
- `eio` direct-style async reads like the original TypeScript
- The `anthropic` opam package (v0.1.0) handles SSE streaming natively
- Shelling out to `rg`/`fd` neutralized Rust's crate ecosystem advantage
- Faster iteration for a port where the architecture is known

OCaml ecosystem highlights discovered via research: `eio` (OCaml 5, production — Docker Desktop, Jane Street), `cohttp-eio`, `ocaml-xxhash`, `yojson`/`jsonm`, `re` (pure OCaml regex), `digestif` (SHA-256), `cmdliner`. Gap: PTY needs C FFI.

### Round 3: Rust — Final Decision

**The GUI requirement reversed the decision.** The vision for rho includes a graph-based conversation viewer:
- See conversation history as a visual tree/graph
- Click into any node to see prompts, tool calls, full flow
- Fork from any point in the conversation
- Smooth pan/zoom on the conversation graph

This is a substantial, differentiating UI feature — not a trivial add-on.

**Why Rust wins with a GUI in scope:**

| Factor | Rust + Iced | OCaml GUI options |
|--------|------------|-------------------|
| GPU-accelerated rendering | Yes (wgpu) | No |
| Elm architecture (fits agent events) | Native to Iced | N/A |
| Graph layout / custom widgets | Iced canvas + widget tree | `lablgtk3` (dated), `bogue` (limited) |
| Single static binary (agent + GUI) | Yes | Possible but harder |
| Smooth pan/zoom on conversation graph | Iced canvas is built for this | Would need SDL2/OpenGL bindings |
| Cross-platform | Excellent | GTK is painful on macOS/Windows |
| Shared types (AgentEvent in GUI) | Zero-cost, same enum | Serialization boundary |

**Key insight:** The GUI doesn't have to be in the agent binary — it can be a separate crate in the same Cargo workspace. But sharing the type system (AgentEvent, Message, ToolCall) between agent core and GUI without a serialization boundary is a massive architectural advantage. Iced's Elm architecture maps naturally to an event-stream-driven agent — the same `AgentEvent` enum that drives the CLI output drives the graph visualization.

OCaml's GUI story is weak: `lablgtk3` feels dated, `bogue` (SDL2) is limited, neither supports custom GPU-accelerated graph rendering. A web UI via `dream` + browser is possible but means writing JS/TS for the frontend.

**Rust also regains its native tool advantages:**
- `ignore` + `globset` + `grep-searcher` = ripgrep internals as libraries (no shelling out needed)
- `FAST-TOOLS-AND-HASHLINE.md` was written specifically for Rust with crate selections already done
- Configurable tool backends can still be offered as an option, but native is the default

## Cargo Workspace Structure

```
rho/
  Cargo.toml              # workspace root
  crates/
    rho-core/             # Agent loop, types, event stream
      src/
        lib.rs
        agent_loop.rs     # Core loop (~400 LOC equivalent)
        types.rs          # AgentEvent, Message, AgentTool, etc.
        event_stream.rs   # Async event stream
    rho-hashline/         # Hashline system (standalone crate)
      src/
        lib.rs
        hash.rs           # xxHash32 computation, lookup table
        format.rs         # LINE:HASH|content formatting
        edit.rs           # Edit operations (set_line, replace_lines, etc.)
        apply.rs          # Application algorithm + heuristic cleanup
        stream.rs         # Streaming hashline generation
    rho-tools/            # Tool implementations
      src/
        lib.rs
        read.rs           # Read tool (hashline formatting, images, dirs)
        edit.rs           # Edit tool (hashline/replace/patch modes)
        write.rs          # Write tool
        bash.rs           # Bash tool (PTY, timeouts)
        grep.rs           # Grep tool (ripgrep internals or configurable backend)
        find.rs           # Find tool (glob, gitignore-aware)
        backend.rs        # Configurable tool backend system
    rho-provider/         # Anthropic API client + SSE streaming
      src/
        lib.rs
        sse.rs            # SSE event parsing
        streaming.rs      # Content block streaming, JSON arg parsing
        models.rs         # Model definitions, thinking levels
    anthropic-auth/       # Standalone OAuth crate (also a CLI)
      src/
        lib.rs
        oauth.rs          # PKCE flow
        token.rs          # Token management, refresh, caching
        config.rs         # Credential storage
      src/bin/
        main.rs           # CLI: anthropic-auth login/token/status
    rho-gui/              # GUI crate (separate, depends on rho-core)
      src/
        lib.rs
        graph.rs          # Conversation graph visualization
        node.rs           # Node rendering (prompt, tool call, response)
        interaction.rs    # Pan, zoom, click, fork
  src/
    main.rs               # CLI entry point
  reference/              # TypeScript reference implementation
```

### Crate dependency graph:

```
rho (CLI binary)
  ├── rho-core
  │    └── rho-provider
  │         └── anthropic-auth
  ├── rho-tools
  │    ├── rho-core (types)
  │    └── rho-hashline
  └── rho-gui (optional feature)
       └── rho-core (types, events)

anthropic-auth (standalone CLI binary)
  └── (no rho dependencies — fully independent)
```

## Rust Crate Selections

From `FAST-TOOLS-AND-HASHLINE.md` + additional selections:

### Core agent
| Need | Crate | Notes |
|------|-------|-------|
| Async runtime | `tokio` | Full-featured, required by reqwest |
| HTTP client | `reqwest` | SSE via chunked streaming |
| SSE parsing | `eventsource-stream` | Or hand-roll over reqwest stream |
| JSON | `serde` + `serde_json` | Streaming via `serde_json::StreamDeserializer` |
| CLI | `clap` | Derive macros, completions |
| Config (TOML) | `toml` + `serde` | For tool backend config |

### Hashline system
| Need | Crate | Notes |
|------|-------|-------|
| xxHash32 | `xxhash-rust` (xxh32 feature) | Exact match to reference impl |
| Memory mapping | `memmap2` | Large file streaming |
| Fuzzy matching | `nucleo-matcher` | For `replace` fallback mode |

### File tools
| Need | Crate | Notes |
|------|-------|-------|
| Gitignore-aware walk | `ignore` | From BurntSushi (ripgrep author) |
| Glob matching | `globset` | From BurntSushi |
| Grep engine | `grep-searcher` + `grep-regex` | Literal ripgrep internals |
| Regex | `regex` | From BurntSushi |
| PTY | `portable-pty` or `tokio-pty-process` | For bash tool |

### Auth (anthropic-auth crate)
| Need | Crate | Notes |
|------|-------|-------|
| SHA-256 (PKCE) | `sha2` | For code challenge |
| Base64 | `base64` | URL-safe encoding for PKCE |
| HTTP (token exchange) | `reqwest` | OAuth token endpoint |
| File locking | `fd-lock` or `fs2` | Token refresh race prevention |
| Browser open | `open` | Launch OAuth authorize URL |
| Local HTTP server | `tiny_http` or `warp` | OAuth callback listener |

### GUI (rho-gui crate)
| Need | Crate | Notes |
|------|-------|-------|
| GUI framework | `iced` | Elm architecture, GPU-accelerated |
| Graph layout | `iced` canvas widget | Custom rendering |

## Configurable Tool Backends

### Design: User-Configurable CLI Tool Mapping

While Rust gives us native access to ripgrep internals, we still want users to be able to configure alternative backends. This is useful for:
- Users who prefer specific tools (e.g., `ag` over `rg`)
- Environments where certain tools aren't available
- Custom tools for specialized workflows

### Configuration File

Location: `~/.config/rho/tools.toml` (or `~/.rho/tools.toml`)

```toml
# Tool backend configuration
# Each tool can use "native" (built-in Rust implementation) or an external binary
# Default: native for grep/find, external for others

[grep]
backend = "native"      # Use ripgrep internals (default)
# backend = "external"
# binary = "rg"         # ripgrep CLI
# binary = "ag"         # silver searcher

[find]
backend = "native"      # Use ignore + globset (default)
# backend = "external"
# binary = "fd"         # fd-find

[ls]
binary = "ls"           # standard ls (default)
# binary = "eza"        # alternative: modern ls replacement

[diff]
binary = "diff"         # standard diff (default)
# binary = "delta"      # alternative: syntax-highlighted diff
```

### Architecture

```
Tool Request (from LLM)
  → Tool Handler (Rust)
    → Check backend config
      → "native": call ripgrep/ignore crate directly
      → "external": resolve binary path → build args → execute subprocess
    → Parse output (unified internal format)
      → Format for LLM (hashline, etc.)
```

Each tool backend defines:

1. **A capability trait** — what the tool handler needs (pattern, path, context lines, etc.)
2. **A native implementation** — using Rust crates directly
3. **An external implementation** — argument builder + output parser per binary

```rust
/// Trait for grep backend implementations
trait GrepBackend {
    async fn search(&self, req: GrepRequest) -> Result<Vec<GrepMatch>>;
}

/// Native: uses grep-searcher + grep-regex crates
struct NativeGrep;

/// External: shells out to rg, ag, grep, etc.
struct ExternalGrep {
    binary: String,
    extra_args: Vec<String>,
}
```

### Auto-Detection

On first run (or when config is missing), auto-detect and write defaults:

```
1. Native backends are always available (compiled in)
2. For external backends, check PATH: rg > ag > grep, fd > find
3. Write discovered config to tools.toml
4. Log which backends were selected
```

## Anthropic Auth — Standalone Crate Design

The `anthropic-auth` crate handles OAuth + API key auth as a fully independent package:

### As a standalone CLI:

```bash
# First-time OAuth login
anthropic-auth login

# Get current token (for piping to other tools)
anthropic-auth token

# Set as environment variable
export ANTHROPIC_API_KEY=$(anthropic-auth token)

# Check auth status
anthropic-auth status
```

### As a library dependency in rho:

```rust
// In rho's agent setup
let api_key = anthropic_auth::get_token().await?;
// Uses cached token, refreshes if expired
```

### Functionality:
- PKCE OAuth flow (browser-based)
- Token caching at `~/.config/anthropic-auth/auth.json`
- Automatic token refresh with file locking
- API key passthrough (if `ANTHROPIC_API_KEY` is set, just use it)
- OAuth token detection (`sk-ant-oat` prefix)

### Crate structure:
```
anthropic-auth/
  src/
    lib.rs        # Public API: get_token(), login(), status()
    oauth.rs      # PKCE flow (sha2, base64, browser open)
    token.rs      # Token management, refresh, caching
    config.rs     # Credential storage (~/.config/anthropic-auth/)
  src/bin/
    main.rs       # CLI entry point (clap)
  Cargo.toml      # Independent — no rho dependencies
```

## Open Questions

1. ~~**How important is "no shelling out"?**~~ **Resolved**: Native Rust crates are the default; configurable backends allow shelling out as an option.
2. **Is the hashline system required for v1?** Could start with simpler `old_text`/`new_text` replace mode and add hashline later.
3. ~~**How important is binary size/startup?**~~ Rust handles this well by default.
4. ~~**Is Cortex integration a goal?**~~ Not a primary goal.
5. **Multi-provider support?** The reference supports OpenAI, Google, Bedrock, etc. — is Anthropic-only sufficient for v1?
6. **What's the target for "done"?** Minimal viable agent (loop + 4 tools + API key auth) vs full feature parity?
7. **Tool backend config location?** `~/.config/rho/tools.toml` vs `~/.rho/tools.toml` vs project-local `.rho/tools.toml`?
8. **GUI timeline?** Is `rho-gui` a v1 requirement or a v2 feature? Workspace structure supports either.
9. **Iced version?** Iced 0.13+ has significant API changes — need to target the right version.
