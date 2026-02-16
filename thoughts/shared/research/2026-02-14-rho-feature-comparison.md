---
date: 2026-02-14T12:00:00-08:00
researcher: reuben
git_commit: 321fef6032916646bae3991d5ebb6e8601e58e6b
branch: main
repository: rho
topic: "Feature comparison: Rho vs Claude Code vs OpenCode vs HumanLayer"
tags: [research, feature-comparison, rho, claude-code, opencode, humanlayer]
status: complete
last_updated: 2026-02-14
last_updated_by: reuben
---

# Research: Feature Comparison — Rho vs Claude Code vs OpenCode vs HumanLayer

**Date**: 2026-02-14
**Git Commit**: 321fef6
**Branch**: main
**Repository**: rho

## Research Question

Feature comparison between Rho (this codebase), Claude Code (Anthropic's CLI), OpenCode (open-source coding agent), and HumanLayer (human-in-the-loop infrastructure).

## Summary

Rho is a Rust-native coding agent with a native desktop GUI, currently implementing a feature set comparable to ~40-50% of Claude Code's surface area. OpenCode is a Go/TypeScript multi-provider alternative with 75+ LLM support. HumanLayer is complementary infrastructure (not a competing agent) providing human-in-the-loop approval workflows. Below is a detailed matrix.

---

## Feature Matrix

### Core Agent Features

| Feature | Rho | Claude Code | OpenCode | HumanLayer |
|---------|-----|-------------|----------|------------|
| **Language** | Rust | TypeScript/Node | Go + TypeScript (Bun) | TypeScript + Go |
| **License** | — | Proprietary | MIT (archived → Crush) | Apache 2.0 |
| **Type** | Coding agent + GUI | Coding agent | Coding agent | HITL infrastructure |
| **Binary count** | 3 (rho, anthropic-auth, rho-gui) | 1 (claude) | 1 (opencode) | Daemon + CLI + Desktop |
| **Binary size** | Native Rust (~small) | Node.js bundle | Go binary | N/A |
| **Startup time** | Fast (native) | ~1-2s (Node) | Fast (Go) | N/A |

### Tools

| Tool | Rho | Claude Code | OpenCode | HumanLayer |
|------|-----|-------------|----------|------------|
| **Read file** | read (LINE:HASH format) | Read | read | N/A |
| **Write file** | write | Write | write | N/A |
| **Edit file** | edit (hashline + replace) | Edit (search/replace) | edit (exact string replace) | N/A |
| **Bash/shell** | bash (PTY, timeout) | Bash (persistent session) | bash | N/A |
| **Search content** | grep (ripgrep internals) | Grep (ripgrep) | grep | N/A |
| **Find files** | find (ignore + globset) | Glob | glob | N/A |
| **Web fetch** | — | WebFetch | webfetch | N/A |
| **Web search** | — | WebSearch | websearch (Exa AI) | N/A |
| **Directory list** | read (dir mode) | LS | list | N/A |
| **Patch/diff** | — | — | patch | N/A |
| **LSP** | — | — | lsp (experimental) | N/A |
| **Task/subagent** | — | Task (subagents) | — | Agent Control Plane |
| **Ask user** | — | AskUserQuestion | question | Core feature |
| **Todo/task list** | — | TodoWrite | todowrite/todoread | — |
| **Skill invoke** | — | Skill | skill | — |
| **MCP tools** | — | Dynamic via MCP | Dynamic via MCP | MCP server support |

### Editing Approach

| Aspect | Rho | Claude Code | OpenCode |
|--------|-----|-------------|----------|
| **Primary method** | Hashline anchors (LINE:HASH) | Search/replace strings | Exact string replacement |
| **Hash integrity** | xxHash32 per line | N/A | N/A |
| **Line drift handling** | Auto-relocation | N/A | N/A |
| **Heuristic cleanup** | 7 heuristics (strip prefixes, boundary echo, indentation, etc.) | N/A | LSP diagnostics feedback |
| **Mismatch errors** | Shows changed lines with >>> markers | Retries with new content | N/A |
| **Fallback mode** | Text replace (old→new) | Whole file rewrite | write tool |
| **Format on save** | — | — | Configurable formatters |

### Authentication

| Feature | Rho | Claude Code | OpenCode |
|---------|-----|-------------|----------|
| **API key** | ANTHROPIC_API_KEY env var | ANTHROPIC_API_KEY | Per-provider keys |
| **OAuth (PKCE)** | Claude Code OAuth flow | Full OAuth + refresh | /connect command |
| **macOS Keychain** | Reads Claude Code tokens | Stores tokens | — |
| **Standalone auth CLI** | anthropic-auth binary | Built-in | — |
| **Multi-provider** | Anthropic only | Anthropic + Azure/Bedrock/Vertex | 75+ providers |
| **Local models** | — | — | LM Studio, OpenAI-compatible |

### UI / Interface

| Feature | Rho | Claude Code | OpenCode |
|---------|-----|-------------|----------|
| **CLI** | Basic (prompt + stream) | Full REPL with history | Full REPL |
| **TUI** | — | — | Bubble Tea TUI |
| **Native GUI** | Iced desktop app | — | Tauri desktop app |
| **VS Code** | — | Extension | ACP extension |
| **JetBrains** | — | Plugin | ACP extension |
| **Web** | — | claude.ai | Web interface |
| **Mobile** | — | iOS app | — |
| **Desktop app** | rho-gui (Iced/Rust) | Standalone app | Tauri app |

### GUI-Specific Features (Rho)

| Feature | Status |
|---------|--------|
| **Conversation view** | Scrollable markdown blocks |
| **Streaming text** | Real-time word-by-word |
| **Markdown rendering** | iced::widget::markdown with syntax highlighting |
| **Tool call blocks** | Collapsible with expand/collapse |
| **Shell mode** | `!` prefix with amber border, bold prefix |
| **Autocomplete** | `/skills` and `@files` with popup |
| **Command history** | Up/Down with draft preservation |
| **Sidebar** | Project, model, tokens, session time |
| **Theme** | TokyoNight |
| **Fonts** | Inter (UI) + JetBrains Mono (code) |
| **Focus management** | Auto-focus on send, mode switch |

### Model Support

| Feature | Rho | Claude Code | OpenCode |
|---------|-----|-------------|----------|
| **Default model** | Sonnet 4.5 | Opus 4.6 (Max), Sonnet 4.5 (API) | Configurable |
| **Model switching** | --model flag | Aliases (sonnet, opus, haiku) | /models command |
| **Extended thinking** | ThinkingLevel enum (Off→High) | Supported | Supported |
| **Streaming** | SSE with text/thinking/tool deltas | SSE streaming | SSE streaming |
| **Context window** | 200K (configurable) | 200K, 1M with [1m] | Provider-dependent |
| **Hybrid mode** | — | opusplan (Opus plan + Sonnet execute) | — |

### Skills / Commands

| Feature | Rho | Claude Code | OpenCode |
|---------|-----|-------------|----------|
| **Skill discovery** | .skills/, .claude/skills/, .opencode/skills/ | .claude/skills/, ~/.claude/skills/ | .opencode/skills/, .claude/skills/ |
| **Skill format** | SKILL.md with YAML frontmatter | SKILL.md with YAML frontmatter | SKILL.md with YAML frontmatter |
| **Slash commands** | /skill autocomplete | /skill with full menu | /skill |
| **@file mentions** | @file with autocomplete + inline | @file with line ranges | @file with fuzzy search |
| **Reference resolution** | XML content blocks at send time | Inline context injection | Inline injection |
| **Dynamic context** | — | `!`command`` in skills | — |
| **Skill hooks** | — | Pre/post hooks in frontmatter | — |

### Context Management

| Feature | Rho | Claude Code | OpenCode |
|---------|-----|-------------|----------|
| **Auto-compaction** | — (planned) | At 98% of context window | At ~90% context |
| **Manual compact** | — | — | ctrl+x c |
| **Summarization** | — | Earlier messages summarized | Auto-summarization |
| **Memory tool** | — | File-based persistent memory | — |
| **Subagent context** | — | Separate context per subagent | Separate context |

### Multi-Agent / Orchestration

| Feature | Rho | Claude Code | OpenCode | HumanLayer |
|---------|-----|-------------|----------|------------|
| **Subagents** | — | Task tool (Explore, Plan, general) | Build, Plan, Explore agents | — |
| **Agent teams** | — | Multi-session with shared task list | — | MultiClaude via CodeLayer |
| **Inter-agent comms** | — | SendMessage between teammates | — | Agent Control Plane |
| **Background tasks** | — | Async subagent execution | — | Async approval workflows |
| **Custom agents** | — | .claude/agents/ definitions | opencode agent create | — |

### Permissions / Security

| Feature | Rho | Claude Code | OpenCode |
|---------|-----|-------------|----------|
| **Permission model** | None (all allowed) | Tiered (allow/ask/deny) | Tiered permissions |
| **Sandbox** | — | OS-level (Seatbelt/bubblewrap) | — |
| **Network isolation** | — | Domain-level proxy | — |
| **Managed settings** | — | Enterprise admin controls | — |
| **File path restrictions** | — | Gitignore-style patterns | Configurable |

### Hooks / Lifecycle

| Feature | Rho | Claude Code | OpenCode |
|---------|-----|-------------|----------|
| **Hook system** | — | 15+ lifecycle events | — |
| **Hook types** | — | Command, Prompt, Agent | — |
| **Pre-tool hooks** | — | PreToolUse (can block) | — |
| **Post-tool hooks** | — | PostToolUse, PostToolUseFailure | — |
| **Session hooks** | — | SessionStart, SessionEnd | — |

### Git Integration

| Feature | Rho | Claude Code | OpenCode |
|---------|-----|-------------|----------|
| **Commit** | Via bash tool | Native git operations | Via bash tool |
| **PR creation** | Via bash tool (gh) | Native with templates | Via bash tool |
| **Co-authored-by** | Manual | Automatic attribution | — |
| **Branch management** | Via bash | Natural language commands | Via bash |
| **Conflict resolution** | — | Intelligent assistance | — |
| **Worktrees** | — | Supported | — |

### Configuration

| Feature | Rho | Claude Code | OpenCode |
|---------|-----|-------------|----------|
| **Project config** | — | CLAUDE.md + .claude/settings.json | opencode.json |
| **Global config** | — | ~/.claude/settings.json | ~/.config/opencode/opencode.json |
| **Config format** | — | JSON + Markdown | JSON/JSONC |
| **Schema validation** | — | — | JSON Schema at opencode.ai/config.json |
| **Variable substitution** | — | — | {env:VAR}, {file:path} |

### MCP (Model Context Protocol)

| Feature | Rho | Claude Code | OpenCode |
|---------|-----|-------------|----------|
| **MCP support** | — | Full (stdio, HTTP, SSE) | Full (local, remote) |
| **Tool search** | — | Dynamic (>10% context threshold) | Glob-pattern filtering |
| **OAuth for MCP** | — | Supported | Automatic/pre-registered/manual |
| **Config locations** | — | .mcp.json, ~/.claude.json | opencode.json |

---

## HumanLayer: Complementary Platform

HumanLayer is **not a competing coding agent** — it's human-in-the-loop infrastructure that works WITH agents. Key differentiators:

| Feature | Description |
|---------|-------------|
| **@hl.require_approval()** | Decorator blocks execution until human approves |
| **human_as_tool()** | Agents can ask humans questions mid-workflow |
| **Omnichannel** | Slack, Email, Web, CLI, Discord, SMS approval channels |
| **Stateless** | Works with Lambda/serverless; approvals survive process death |
| **CodeLayer** | "Post-IDE IDE" — orchestrates multiple Claude Code sessions |
| **ACE methodology** | Advanced Context Engineering for complex codebases |
| **Agent Control Plane** | Kubernetes-native agent orchestrator |
| **Framework support** | LangChain, CrewAI, ControlFlow, Mastra, Vercel AI SDK |

---

## Rho's Unique Differentiators

1. **Native Rust binary** — No runtime dependencies (Node.js, Go, Python)
2. **Iced desktop GUI** — Native GPU-accelerated UI, not Electron/Tauri/WebView
3. **Hashline editing** — xxHash32 integrity checks with auto-relocation and 7 heuristic cleanups
4. **Font bundling** — Inter + JetBrains Mono embedded in binary
5. **Shell mode UX** — `!` prefix with visual amber border, bold prefix indicator
6. **Interactive autocomplete** — `/skills` and `@files` with popup, Tab/Enter accept, arrow navigation
7. **Claude Code OAuth compatibility** — Reads tokens from macOS Keychain

## Feature Gap Summary (Rho vs Claude Code)

**Rho has, Claude Code doesn't:**
- Native desktop GUI with Iced
- Hashline editing with integrity checks
- Bundled fonts

**Claude Code has, Rho doesn't yet:**
- MCP server support
- Subagents / agent teams
- Hook system (15+ lifecycle events)
- Sandboxing (OS-level)
- Permission model (allow/ask/deny)
- Context compaction / auto-summarization
- Memory tool
- Web fetch / web search
- IDE integrations (VS Code, JetBrains)
- Mobile / web access
- Session teleportation
- Git-aware operations
- CLAUDE.md project configuration
- TodoWrite task management
- AskUserQuestion tool
- Plugin system

---

## Code References

- `crates/rho-core/src/agent_loop.rs` — Agent loop with streaming, tool execution, steering
- `crates/rho-core/src/skills.rs` — Skill discovery from .skills/, .claude/skills/, .opencode/skills/
- `crates/rho-tools/src/` — 6 tools: read, write, edit, bash, grep, find
- `crates/rho-hashline/src/` — Hash computation, edit operations, apply algorithm, heuristics
- `crates/rho-gui/src/app.rs` — GUI state, messages, event handling, shell mode
- `crates/rho-gui/src/view.rs` — Layout, sidebar, chat, autocomplete popup, font constants
- `crates/rho-gui/src/autocomplete.rs` — Trigger detection, suggestions, reference resolution
- `crates/rho-gui/src/main.rs` — Iced application setup with font loading
- `crates/rho-provider/src/lib.rs` — Anthropic SSE streaming with OAuth headers
- `crates/anthropic-auth/src/lib.rs` — Token resolution (env, Keychain, OAuth)
- `crates/anthropic-auth/src/oauth.rs` — PKCE OAuth flow
- `src/main.rs` — CLI entry point with clap args

## Related Research

- `thoughts/shared/research/2026-02-13-pycodingagent-port-research.md` — Original port research
- `thoughts/shared/plans/2026-02-13-rho-rust-port.md` — Implementation plan (9 phases)

## Open Questions

- Should Rho prioritize MCP support or context management next?
- Is multi-provider support (like OpenCode) desirable, or stay Anthropic-only?
- Should Rho adopt the Agent Skills open standard for cross-tool compatibility?
- Would a permission model (like Claude Code's allow/ask/deny) be valuable?
