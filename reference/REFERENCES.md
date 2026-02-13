# Reference Material for Rust Port

## Blog Posts

- **Pi Coding Agent Philosophy** — Mario Zechner (2025-11-30)
  https://mariozechner.at/posts/2025-11-30-pi-coding-agent/
  Core design manifesto: radical minimalism, 4 tools, <1000 token system prompt, no MCP/plan mode/sub-agents. Models already know how to code via RL training.

- **The Harness Problem** — Can Bölük (2026-02-12)
  https://blog.can.ac/2026/02/12/the-harness-problem/
  The case for hashline editing. Benchmarks show +900% improvement for some models, ~20% fewer output tokens. Patch is worst format for nearly every model; hashline matches or beats string-replace.

## Source Repositories

- **pi-mono** (upstream): Mario Zechner's pi coding agent
  https://github.com/nickarellano/pi — (or wherever the canonical repo lives)
  The clean, minimal implementation. Four tools, streaming-first, cross-provider support.

- **Fork** (Can Bölük): Adds hashline editing, LSP integration, MCP, plugins, custom commands, etc.
  Source for the `fork/` files extracted from `repomix-output.xml`.

## File Inventory

### `upstream/` — From pi-mono (this repo)

| File | Source | What It Contains |
|------|--------|-----------------|
| `anthropic-provider.ts` | `packages/ai/src/providers/anthropic.ts` | Anthropic Messages API streaming, OAuth detection, Claude Code identity headers, tool name normalization |
| `ai-types.ts` | `packages/ai/src/types.ts` | Core types: Message, Content, Tool, Model, StreamOptions, Usage |
| `stream.ts` | `packages/ai/src/stream.ts` | `stream()`, `complete()`, `streamSimple()` entry points |
| `event-stream.ts` | `packages/ai/src/utils/event-stream.ts` | `AssistantMessageEventStream` — the typed event emitter for streaming |
| `env-api-keys.ts` | `packages/ai/src/env-api-keys.ts` | Environment variable → provider API key mapping |
| `oauth-anthropic.ts` | `packages/ai/src/utils/oauth/anthropic.ts` | Anthropic OAuth PKCE flow (claude.ai → console.anthropic.com) |
| `oauth-types.ts` | `packages/ai/src/utils/oauth/types.ts` | `OAuthCredentials`, `OAuthProviderInterface` |
| `auth-storage.ts` | `packages/coding-agent/src/core/auth-storage.ts` | `AuthStorage` class — credential loading, saving, refresh with file locking |
| `agent-loop.ts` | `packages/agent/src/agent-loop.ts` | **THE core loop**: message → LLM → tool dispatch → repeat |
| `agent.ts` | `packages/agent/src/agent.ts` | `Agent` class wrapping the loop with state, events, abort |
| `agent-types.ts` | `packages/agent/src/types.ts` | `AgentTool`, `AgentMessage`, tool result types |

### `upstream/claude-code-auth/` — Claude Code OAuth Authentication

Complete auth flow for using a Claude Pro/Max subscription. See `CLAUDE-CODE-AUTH.md` in that directory for the full walkthrough.

| File | Source | What It Contains |
|------|--------|-----------------|
| `CLAUDE-CODE-AUTH.md` | (written) | Full walkthrough: flow diagrams, constants, token format, what to port |
| `oauth-flow.ts` | `packages/ai/src/utils/oauth/anthropic.ts` | PKCE login flow + token refresh against claude.ai |
| `oauth-types.ts` | `packages/ai/src/utils/oauth/types.ts` | `OAuthCredentials`, `OAuthProviderInterface` |
| `oauth-registry.ts` | `packages/ai/src/utils/oauth/index.ts` | Provider registry, `getOAuthProvider()`, `getOAuthApiKey()` with auto-refresh |
| `pkce.ts` | `packages/ai/src/utils/oauth/pkce.ts` | PKCE verifier + SHA-256 challenge generation |
| `auth-storage.ts` | `packages/coding-agent/src/core/auth-storage.ts` | `AuthStorage` class — file-backed credentials with locked refresh |
| `env-api-keys.ts` | `packages/ai/src/env-api-keys.ts` | `ANTHROPIC_OAUTH_TOKEN` takes precedence over `ANTHROPIC_API_KEY` |

The OAuth token detection + Claude Code identity header injection lives in `upstream/anthropic-provider.ts` — see `createClient()` at line 486 and `isOAuthToken()` at line 482.

### `fork/` — Extracted from repomix-output.xml

| File | Source | What It Contains |
|------|--------|-----------------|
| `hashline.ts` | `packages/coding-agent/src/patch/hashline.ts` | **THE key file**: hash generation (xxHash32), line formatting, ref parsing, validation, `applyHashlineEdits()`, mismatch errors, heuristic cleanup |
| `edit-tool.ts` | `packages/coding-agent/src/patch/index.ts` | Edit tool class with hashline/replace/patch mode switching, schema definitions, execution logic |
| `read-tool.ts` | `packages/coding-agent/src/tools/read.ts` | Read tool with hashline output formatting (`LINE:HASH\|content`) |
| `write-tool.ts` | `packages/coding-agent/src/tools/write.ts` | Write tool (simple file creation) |
| `bash-tool.ts` | `packages/coding-agent/src/tools/bash.ts` | Bash execution with timeout, PTY support, streaming output |
| `grep-tool.ts` | `packages/coding-agent/src/tools/grep.ts` | Grep tool with hashline-formatted match output |
| `find-tool.ts` | `packages/coding-agent/src/tools/find.ts` | Glob-based file finder |
| `file-display-mode.ts` | `packages/coding-agent/src/utils/file-display-mode.ts` | Resolves when hashlines are enabled (setting/env var) |

## Key Sections to Study

For the Rust port, read these in order:

1. **`upstream/agent-loop.ts`** + **`upstream/agent.ts`** — The ~400 line core loop. This is the heart of everything.
2. **`fork/hashline.ts`** — The hashline system. `computeLineHash()` (line ~245), `formatHashLines()` (line ~262), `parseLineRef()` (line ~505), `applyHashlineEdits()` (line ~635).
3. **`upstream/anthropic-provider.ts`** — How to call Claude. `createClient()` for auth branching, `streamAnthropic()` for SSE event handling.
4. **`upstream/claude-code-auth/CLAUDE-CODE-AUTH.md`** — The full Claude Code OAuth flow. Start here for auth, then read the source files.
5. **`fork/edit-tool.ts`** — The edit schema definitions and how hashline edits are wired.
6. **`fork/read-tool.ts`** — How files are formatted with hashline prefixes for model consumption.

## Architecture Notes

See also: `thoughts/shared/research/2026-02-13-rust-port-architecture-research.md` for full architecture analysis, crate structure proposal, and implementation phasing.
