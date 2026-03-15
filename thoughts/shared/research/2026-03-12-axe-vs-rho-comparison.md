---
date: 2026-03-12T23:30:00-07:00
researcher: reuben
git_commit: a140498cbefa548c1a89f509fa9f9b55a780fa62
branch: main
repository: rho
topic: "Compare and contrast jrswab/axe with rho"
tags: [research, codebase, comparison, axe, rho, ai-agent, cli]
status: complete
last_updated: 2026-03-12
last_updated_by: reuben
---

# Research: Compare and Contrast jrswab/axe with rho

**Date**: 2026-03-12T23:30:00-07:00
**Researcher**: reuben
**Git Commit**: a140498cbefa548c1a89f509fa9f9b55a780fa62
**Branch**: main
**Repository**: rho

## Research Question
How does jrswab/axe compare to rho — what are the similarities, differences, and trade-offs?

## Summary

Axe and rho are both CLI-based LLM agent tools, but they target fundamentally different use cases. **Axe** is a lightweight, Unix-philosophy task runner for single-purpose agents defined in TOML. **Rho** is a full coding agent with an autonomous loop, session persistence, streaming, extended thinking, and a richer tool set. They share a lot of architectural DNA (multi-provider, tool calling, TOML config) but diverge sharply in scope, runtime model, and complexity.

## Side-by-Side Comparison

| Dimension | **Axe** | **Rho** |
|---|---|---|
| **Language** | Go 1.25 | Rust (2021 edition) |
| **Codebase size** | ~7,000 LOC (non-test) | ~18,000 LOC (non-test) |
| **Direct dependencies** | 3 (cobra, toml, MCP go-sdk) | 20+ (tokio, reqwest, rusqlite, clap, etc.) |
| **Binary philosophy** | Single static binary, ~12 MB | Single binary, larger (Rust + bundled SQLite) |
| **License** | Apache-2.0 | MIT |
| **Primary use case** | Single-purpose task agents (code review, log analysis, commit messages) | Interactive/autonomous coding agent (multi-step, session-based) |
| **Interaction model** | One-shot or pipe-based; no interactive REPL | Interactive REPL + one-shot + autonomous loop mode |
| **Agent definition** | TOML files in `$XDG_CONFIG_HOME/axe/agents/` | Per-project `RHO.md` / `CLAUDE.md` with YAML frontmatter |
| **Runtime model** | Stateless runs, memory appended between runs | Stateful sessions persisted in SQLite |

## Detailed Findings

### 1. Provider Support

| Provider | Axe | Rho |
|---|---|---|
| Anthropic (native API) | Yes — Messages API, non-streaming | Yes — Messages API, SSE streaming |
| OpenAI-compatible | Yes — Chat Completions, non-streaming | Yes — Chat Completions, SSE streaming |
| Ollama | Yes — dedicated provider | Yes — via OpenAI-compatible layer |
| xAI (Responses API) | No | Yes — dedicated `xai_responses` provider |
| OAuth / token auth | No | Yes — `anthropic-beta: oauth-2025-04-20` |

**Key difference**: Axe uses non-streaming HTTP calls (`io.ReadAll` on response body). Rho uses SSE streaming throughout, with an `EventStream` abstraction that produces `AssistantStreamEvent`s. This means rho can display partial output as it arrives while axe waits for the full response.

Both implement tool calling natively against each provider's wire format (Anthropic content blocks, OpenAI function calling, Ollama tool calls). All HTTP calls are hand-rolled against `net/http` (axe) and `reqwest` (rho) — neither uses vendor SDKs.

### 2. Tool System

**Axe built-in tools** (8):
- `list_directory`, `read_file`, `write_file`, `edit_file` — sandboxed to workdir
- `run_command` — shell via `sh -c`
- `url_fetch` — HTTP fetch with HTML stripping (via `golang.org/x/net`)
- `web_search` — web search
- `call_agent` — delegate to sub-agents (auto-injected, not in `tools[]`)

**Rho built-in tools** (9):
- `read` — files + directories, with `LINE:HASH` format
- `write`, `edit` — `LINE:HASH` anchoring or text replacement
- `bash` — PTY-based shell (up to 1 hour timeout)
- `grep`, `find` — regex search and glob find (respects .gitignore)
- `task` — sub-agent in separate context
- `web_fetch`, `web_search` — HTTP fetch + DuckDuckGo search

**Key differences**:
- Rho's `LINE:HASH` editing system (the `rho-hashline` crate) provides hash-based line anchoring for resilient edits in the face of concurrent changes. Axe uses simple find-and-replace.
- Both have web tools (`url_fetch`/`web_search` in axe, `web_fetch`/`web_search` in rho). Rho's `web_search` uses DuckDuckGo with no API key; axe's implementation details are in `internal/tool/`.
- Rho's bash tool runs in a PTY with up to 1-hour timeout. Axe's `run_command` uses `sh -c` with the agent's configured timeout (default 120s).
- Both sandbox file tools to a working directory. Axe explicitly rejects `..` traversal and symlink escapes.

### 3. MCP Support

| | Axe | Rho |
|---|---|---|
| MCP client | Yes — `modelcontextprotocol/go-sdk` | No |
| MCP transport | SSE + streamable-http | N/A |
| Tool routing | `mcpclient.Router` — namespaced by server, dedupes with builtins | N/A |
| MCP server mode | No | No |

Axe has first-class MCP client support. Agents can declare `[[mcp_servers]]` in TOML with name, URL, transport type, and custom headers (with env var interpolation). MCP tools are discovered via `ListTools`, converted to the internal `provider.Tool` format, and routed through a `Router` that handles dispatch and type coercion.

Rho has no MCP support currently.

### 4. Agent Orchestration / Sub-agents

**Axe**: Agents can declare `sub_agents = ["test-runner", "lint-checker"]`. The `call_agent` tool is auto-injected when sub_agents are configured. Sub-agent execution respects:
- `max_depth` (up to 5) — prevents infinite recursion
- `parallel` — concurrent sub-agent execution
- `timeout` — per sub-agent timeout

Each sub-agent is a full agent run: load TOML, resolve context, call LLM. The parent's global config flows through so API keys are shared.

**Rho**: The `task` tool launches a sub-agent in a separate context. Implementation is simpler — it's one of the built-in tools rather than a dedicated orchestration layer.

### 5. Memory / Persistence

**Axe**: Markdown-based memory files appended per run (`## timestamp\n**Task:** ...\n**Result:** ...`). Configurable `last_n` entries loaded into system prompt. LLM-assisted garbage collection via `axe gc <agent>`. Memory path: `$XDG_DATA_HOME/axe/memory/<agent>.md`.

**Rho**: SQLite-backed sessions (`rusqlite`) with full message history. Sessions can be resumed with `--resume <session-id>`. Also supports `memories: true` in project config, plus a `compaction` system that compresses context when approaching token limits.

**Key difference**: Axe's memory is simple, human-readable markdown that persists *across* runs (cross-session knowledge). Rho's sessions are complete conversation replays stored in SQLite (within-session persistence). Rho's compaction handles the "too much context" problem by transforming messages before sending to the LLM.

### 6. Configuration

**Axe** uses two TOML files:
- Per-agent: `$XDG_CONFIG_HOME/axe/agents/<name>.toml` — name, model, system_prompt, skill, tools, sub_agents, memory, params
- Global: `$XDG_CONFIG_HOME/axe/config.toml` — per-provider API keys and base URLs

**Rho** uses:
- Per-project: `RHO.md` or `CLAUDE.md` in project root — YAML frontmatter for model, thinking level, hooks, validation commands + markdown body as system prompt
- Custom models: `~/.rho/models.toml` — model registry with provider, context window, max tokens, thinking flag
- CLI flags for everything else

**Key difference**: Axe is agent-centric (one config file per agent type). Rho is project-centric (one config per project, agents are generic). This reflects their different mental models: axe wants you to create many focused agents, rho wants one powerful agent that adapts per project.

### 7. Extended Thinking / Reasoning

**Axe**: No support. The provider implementations only pass `temperature` and `max_tokens`.

**Rho**: First-class support. `ThinkingLevel` enum (off, minimal, low, medium, high) maps to Anthropic's extended thinking budget and xAI's reasoning parameters. Thinking content is streamed via `AssistantStreamEvent::Thinking` events and optionally displayed on stderr.

### 8. Streaming

**Axe**: Non-streaming only. All providers read the full response body before returning. Output is printed once at the end.

**Rho**: SSE streaming throughout. Uses a custom `EventStream` producer/consumer pattern. The `rho-provider` crate parses SSE events in real-time, producing typed `AssistantStreamEvent`s (text delta, tool use start, thinking, etc.). Agent events are streamed to the frontend via `EventStreamConsumer`.

### 9. Skills System

**Axe**: Skills are `SKILL.md` files following a community format. Resolved from config dir by name or path. Injected into the system prompt.

**Rho**: Has a `skills` module in `rho-core` and supports auto-discovery of project-specific skills and slash commands. Skills are more integrated into the runtime rather than just static prompt injection.

### 10. Testing

**Axe**: Extensive Go test files for every package. Golden tests for CLI output. Integration tests for MCP and full agent runs. Test fixtures with TOML agents.

**Rho**: Standard Rust testing with `#[cfg(test)]` modules. Dev-dependency on `tempfile`.

### 11. Docker / Deployment

**Axe**: Full Docker support with hardened containers (non-root, read-only rootfs, all caps dropped, no privilege escalation). Docker Compose for running with local Ollama sidecar.

**Rho**: Install script for binary download. Pre-built release binaries for Linux/macOS/Windows. No Docker support documented.

### 12. Error Handling

Both categorize provider errors into typed categories (auth, rate limit, timeout, bad request, server error) and map them to exit codes. Axe uses `ErrorCategory` string constants; rho uses Rust enums (`ProviderError`).

## Architecture Diagram

```
┌─────────────────────────────────────────────────────┐
│                        AXE                           │
│  ┌─────────┐  ┌──────────┐  ┌────────────────────┐ │
│  │  TOML   │→ │  run.go  │→ │  provider.Send()   │ │
│  │ configs │  │ (cobra)  │  │  (non-streaming)   │ │
│  └─────────┘  │          │  └────────────────────┘ │
│               │  tool    │  ┌────────────────────┐ │
│               │  loop    │← │  tool.Registry     │ │
│               │  (50     │  │  + MCP Router      │ │
│               │  turns)  │  └────────────────────┘ │
│               └──────────┘                          │
│  Go • ~7k LOC • 12 MB binary • zero-daemon        │
└─────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────┐
│                        RHO                           │
│  ┌─────────┐  ┌──────────┐  ┌────────────────────┐ │
│  │ RHO.md  │→ │  main.rs │→ │  EventStream       │ │
│  │ models  │  │  (clap)  │  │  SSE streaming     │ │
│  │ .toml   │  │          │  └────────────────────┘ │
│  └─────────┘  │ agent_   │  ┌────────────────────┐ │
│               │ loop.rs  │← │  AgentTool trait    │ │
│               │ (async   │  │  (9 built-in tools) │ │
│               │  tokio)  │  └────────────────────┘ │
│               └──────────┘  ┌────────────────────┐ │
│                             │  SQLite sessions   │ │
│                             │  + compaction       │ │
│                             └────────────────────┘ │
│  Rust • ~18k LOC • 6 crates • async/streaming     │
└─────────────────────────────────────────────────────┘
```

## When You'd Choose One Over the Other

**Choose Axe when:**
- You want composable, single-purpose agents triggered from Unix pipelines, cron, git hooks
- You need MCP server integration
- You want sub-agent orchestration with depth limiting
- Minimal dependencies and a tiny static binary matter
- Docker-first deployment is important
- You don't need streaming or interactive sessions

**Choose Rho when:**
- You want an interactive coding agent with session persistence
- Extended thinking / reasoning model support matters
- You need real-time streaming output
- You want autonomous multi-step execution (plan + build loop)
- You need hash-based resilient file editing
- Web search/fetch are needed without external MCP servers
- You want per-project configuration rather than per-agent configuration

## Potential Cross-Pollination Ideas

| From Axe → Rho | From Rho → Axe |
|---|---|
| MCP client support | SSE streaming |
| Sub-agent depth limiting + parallel execution | Extended thinking support |
| Docker hardened containers | SQLite session persistence |
| Skill resolution (bare name → config path) | LINE:HASH editing |
| Dry-run mode for debugging prompts | Compaction for long conversations |
| Refusal detection (`internal/refusal`) | Post-tool hooks + validation commands |
| JSON output envelope with metadata | OAuth / keychain auth |

## Code References

### Axe
- `internal/agent/agent.go` — Agent config struct, TOML parsing, validation
- `internal/provider/provider.go` — Provider interface, Tool/ToolCall/Message types
- `internal/provider/anthropic.go` — Anthropic Messages API (non-streaming)
- `internal/provider/openai.go` — OpenAI Chat Completions API (non-streaming)
- `internal/provider/ollama.go` — Ollama Chat API
- `internal/mcpclient/mcpclient.go` — MCP client (SSE + streamable-http)
- `internal/mcpclient/router.go` — MCP tool routing and dispatch
- `internal/memory/memory.go` — Markdown-based persistent memory
- `cmd/run.go` — Main agent execution loop (50-turn conversation)

### Rho
- `crates/rho-core/src/agent_loop.rs` — Async agent loop with EventStream
- `crates/rho-core/src/config.rs` — RHO.md/CLAUDE.md config loading
- `crates/rho-core/src/tool.rs` — AgentTool trait
- `crates/rho-core/src/models.rs` — Model registry
- `crates/rho-core/src/compaction.rs` — Context compaction
- `crates/rho-core/src/session/mod.rs` — SQLite session persistence
- `crates/rho-provider/src/lib.rs` — Provider dispatch, SSE streaming
- `crates/rho-provider/src/sse.rs` — SSE stream parser
- `crates/rho-provider/src/response.rs` — Response handler for streaming events
- `crates/rho-hashline/` — LINE:HASH file anchoring system

## Related Research
- `thoughts/shared/research/2026-02-14-rho-feature-comparison.md` — Prior feature comparison research

## Open Questions
- Does axe plan to add streaming support?
- Could rho benefit from axe's TOML-based multi-agent orchestration model?
- Would MCP support in rho replace the need for some built-in tools?
