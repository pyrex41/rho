---
date: 2026-02-15T14:00:00-08:00
researcher: reuben
git_commit: 1518f97ea6a7003455ee464b6f2b36ed3aac3dda
branch: main
repository: rho
topic: "Rho Status Review: Progress, Leanness Assessment, and Zig Port Evaluation"
tags: [research, status, leanness, zig, rust, architecture, assessment]
status: complete
last_updated: 2026-02-15
last_updated_by: reuben
---

# Research: Rho Status, Leanness, and Zig Port Evaluation

**Date**: 2026-02-15
**Git Commit**: 1518f97
**Branch**: main
**Repository**: rho

## Research Question

Review all progress and status. Is Rho as lean and clean as it could be? Should we port to Zig?

## Executive Summary

**Progress**: Rho is a functional Rust coding agent at ~12,270 LOC with 7 tools, hashline editing, OAuth auth, a native Iced GUI, autonomous loop mode, context compaction, project config, and built-in commands. Since the initial commit 2 days ago, significant features have been added but not yet committed (compaction, config, task/subagent, loop runner, commands = 1,237 new lines).

**Leanness**: 9/10. Two dead stub crates (`rho-lib`, `rho-cli`) totaling 4 lines. Zero duplication. Clean crate boundaries. 3 minor clippy warnings. The codebase is tight.

**Zig verdict**: No. Zig's async I/O is being redesigned (pre-1.0, no timeline), GUI frameworks are explicitly not production-ready, and SSE streaming libraries don't exist. You'd spend 3-5 months rebuilding what works today, fighting breaking changes the whole time. Stay Rust; use `cargo-zigbuild` for cross-compilation if needed.

---

## Part 1: Current Status & Progress

### Commit History

| Commit | Date | Description |
|--------|------|-------------|
| `1dd514f` | Feb 13 | Initial commit: Phases 1 & 2 (core agent loop, tools, hashline, provider, auth) |
| `67950ff` | Feb 13 | OAuth tokens from macOS Keychain |
| `d76f5fb` | Feb 14 | Wire agent loop, sidebar, shell commands, markdown rendering in GUI |
| `321fef6` | Feb 14 | Autocomplete, shell mode UI, bundled fonts, tools, hashline modules |
| uncommitted | Feb 15 | Compaction, config, task tool, loop runner, commands (1,237 LOC) |

### Crate Structure

| Crate | Lines | Purpose | Status |
|-------|-------|---------|--------|
| `rho-core` | 2,471 | Agent loop, types, config, compaction, commands, skills | Complete |
| `rho-tools` | 3,011 | 7 tools: read, write, edit, bash, grep, find, task | Complete |
| `rho-hashline` | 2,232 | Content-addressed line editing (xxHash32) | Complete |
| `rho-provider` | 1,406 | Anthropic SSE streaming client | Complete |
| `rho-gui` | 1,924 | Iced desktop GUI (markdown, autocomplete, shell mode) | Complete |
| `anthropic-auth` | 550 | OAuth PKCE + macOS Keychain | Complete |
| `src/` (root bin) | 645 | CLI entry + loop runner | Complete |
| `rho-cli` | 3 | **Dead stub** — prints "Hello" | Delete |
| `rho-lib` | 1 | **Dead stub** — empty file | Delete |
| **Total** | **12,269** | | |

### Feature Inventory

**Working today:**
- Single-shot CLI: `rho "prompt"` or `rho --prompt-file FILE`
- Autonomous loop: `rho loop --mode build|plan` (Ralph pattern)
- 7 tools: read (hashline), write, edit (hashline anchors + text replace), bash (PTY), grep (ripgrep), find (gitignore-aware), task (subagent)
- Streaming SSE with text/thinking/tool deltas
- Extended thinking (Off/Minimal/Low/Medium/High)
- OAuth PKCE + macOS Keychain + env var auth
- RHO.md/CLAUDE.md project config (model, thinking, tools, validation commands, compact threshold)
- Context compaction (token estimation, tool result pruning)
- Skill discovery (.skills/, .claude/skills/, .opencode/skills/)
- Built-in commands (/research, /plan, /implement, /validate, /commit)
- Native Iced GUI with markdown rendering, syntax highlighting, tool call expansion
- Autocomplete for /skills and @files
- Shell mode (! prefix)
- Bundled fonts (Inter + JetBrains Mono)

**Not yet implemented:**
- MCP server support
- Permission model (allow/ask/deny)
- Hook system (pre/post tool)
- Web fetch / web search tools
- AskUserQuestion tool
- Multi-turn conversation in CLI (currently single-shot)
- Memory/persistent context
- IDE integrations

### Build Health

| Metric | Result |
|--------|--------|
| `cargo build` | Passes |
| `cargo test` | 0 tests at root level (tests in lib crates) |
| `cargo clippy` | 3 warnings (all `&PathBuf` → `&Path` in main.rs) |
| Release binary (CLI) | **8.4 MB** |
| Release binary (GUI) | **21 MB** (includes bundled fonts) |
| Total Rust LOC | 12,269 |
| External deps (unique) | ~15 beyond workspace-level |

### Uncommitted Files Ready to Commit

All 5 new files are complete, tested, and integrated:

| File | Lines | Tests | Quality |
|------|-------|-------|---------|
| `crates/rho-core/src/commands.rs` | 223 | 6 tests | Clean |
| `crates/rho-core/src/compaction.rs` | 288 | 6 tests | Clean |
| `crates/rho-core/src/config.rs` | 232 | 6 tests | Clean |
| `crates/rho-tools/src/task.rs` | 281 | 3 tests | Clean |
| `src/loop_runner.rs` | 213 | 0 (runtime) | Clean |

---

## Part 2: Leanness Assessment

### Score: 9/10

**What's lean:**
- Zero duplication across crates
- Each crate has a single clear responsibility
- No unnecessary abstraction layers
- Minimal external dependencies (~15 unique beyond workspace)
- High functionality-to-LOC ratio
- No dead feature flags or conditional compilation bloat
- Test coverage where it matters (hashline, provider, config, compaction)

**What could be cleaner (minor):**

1. **Delete `rho-lib`** — 1 line, empty, completely unused
2. **Delete `rho-cli`** — 3 lines, stub "Hello from rho-cli", never used
3. **Fix 3 clippy warnings** — `&PathBuf` → `&Path` in `build_tools()` and `build_system_prompt()` in `src/main.rs`
4. **`rho-core/src/error.rs`** — 16 lines with single `Generic(String)` variant; either expand or remove (most code uses `anyhow` anyway)
5. **Dead `ToolExecutionUpdate` event variant** — defined in `types.rs` but never emitted anywhere

**What's NOT bloat (justified complexity):**
- `rho-hashline` at 2,232 LOC — This is the differentiating feature. 7 heuristics + fuzzy matching + edit validation is the right amount of code for reliable AI-driven file editing
- `rho-gui` at 1,924 LOC — Native GPU-accelerated GUI with markdown rendering, syntax highlighting, and autocomplete. Compact for what it delivers
- `rho-tools` at 3,011 LOC for 7 tools — ~430 LOC average per tool including tests. Lean

### Dependency Audit

**Workspace dependencies** (all necessary):
- `serde/serde_json` — serialization (unavoidable)
- `tokio` — async runtime (unavoidable)
- `reqwest` — HTTP client for API (unavoidable)
- `clap` — CLI args (standard)
- `thiserror/anyhow` — error handling (standard)
- `tracing` — logging (standard)
- `async-trait` — async trait objects (required until Rust stabilizes async-in-trait)
- `futures/tokio-util` — async utilities (standard)
- `regex` — used by grep tool and hashline heuristics
- `similar` — diff algorithm for edit tool
- `bytes` — byte buffer handling for SSE streaming

**Per-crate specifics** (all justified):
- `xxhash-rust` — fast hashing for hashline
- `grep-*` + `ignore` — ripgrep internals for grep tool
- `portable-pty` — PTY for bash tool
- `globset` — glob matching for find tool
- `iced` — GUI framework (the big one: pulls in wgpu, winit, etc.)
- `sha2/base64` — PKCE OAuth challenge
- `open` — browser launch for OAuth

No unnecessary dependencies detected.

---

## Part 3: Should We Port to Zig?

### Verdict: No

### The Case For Zig (What's Appealing)

| Advantage | Impact on Rho |
|-----------|--------------|
| **Compile times** | 10-30x faster clean builds than Rust |
| **Binary size** | Potentially 2-5x smaller (no Rust runtime overhead) |
| **Cross-compilation** | Zero-config for any platform (built-in) |
| **C interop** | `@cImport` for tree-sitter, native GUI backends |
| **Simpler language** | Days to proficiency vs weeks for Rust |
| **No borrow checker** | Less friction for CLI-tool patterns |

Mitchell Hashimoto chose Zig for Ghostty (terminal emulator) and donated $300K to the Zig Foundation. matklad (rust-analyzer creator) praises Zig's simplicity. Neovim chose Zig over Rust for C harmony.

### The Case Against Zig (What Would Break)

#### 1. Async I/O Is Being Redesigned (Dealbreaker)

Zig's async/await keywords were **removed** from the language. They're planned to return as stdlib features, but:
- "Extremely breaking" `std.io` changes are ongoing ("Writergate")
- No stable event loop exists (only experimental `zio`)
- No timeline for stabilization (Zig is pre-1.0, currently 0.15.x)

Rho needs async for: LLM streaming, subprocess management, GUI events, concurrent tool execution. This alone kills the port.

#### 2. No SSE Client Library

Rho's core operation is streaming SSE from Anthropic's API. In Rust: `reqwest` + manual SSE parsing (~200 LOC). In Zig: You'd implement HTTP client + chunked transfer + SSE parsing from scratch. No library exists.

#### 3. GUI Frameworks Aren't Ready

| Zig GUI | Status | Comparison to Iced |
|---------|--------|-------------------|
| Capy | "Explicitly NOT production-ready" | Closest conceptually |
| DVUI | Most mature, immediate-mode | Different paradigm entirely |
| ZigUI | Unclear maturity | Documentation gaps |

Mitchell Hashimoto's approach: Write core in Zig, GUI in SwiftUI (macOS) / GTK (Linux). He **avoided** Zig GUI frameworks entirely.

#### 4. Pre-1.0 Breaking Changes

Andrew Kelley (Zig creator): "I don't know how long it will take" to reach 1.0. Every Zig release brings breaking changes. Your ported code would break on updates until stabilization.

#### 5. Migration Cost Estimate

| Component | Effort | Risk |
|-----------|--------|------|
| Core agent logic (HTTP, JSON, tools) | 2-3 weeks | Low |
| Async/streaming architecture | 4-6 weeks | **High** |
| GUI layer (Iced → ???) | 4-8 weeks | **Very high** |
| **Total** | **3-5 months** | **High uncertainty** |

You'd be rebuilding ~40% of dependencies (SSE client, async runtime, advanced HTTP). This is pioneering, not productive.

#### 6. Only One Zig Coding Agent Exists

**Zaica** — "Zig AI Coding Assistant" — pre-alpha, basic tool calling. That's it. You'd be the second person attempting this.

### What To Do Instead

1. **Stay Rust** — The ecosystem is mature, async works, Iced is production-quality
2. **Use `cargo-zigbuild`** — Get Zig's cross-compilation as Rust's linker (best of both worlds)
3. **Watch Zig milestones** — Async stabilization, Capy maturity, 1.0 release
4. **Consider Zig for a future CLI-only tool** — Where async/GUI gaps don't matter

### When Zig Would Make Sense for This Project

- Zig reaches 1.0 (stable std library, async finalized)
- A Zig GUI framework reaches production quality
- SSE client libraries exist
- You're willing to adopt Mitchell's pattern: Zig core + platform-native GUI

Best estimate: 2028+.

---

## Code References

- `Cargo.toml:1-12` — Workspace definition (8 crates)
- `src/main.rs:115-128` — System prompt constant
- `src/main.rs:130-149` — Tool registration with filtering
- `src/main.rs:151-165` — Model builder
- `src/main.rs:167-213` — System prompt assembly (skills, commands, config)
- `src/main.rs:215-433` — Main entry (CLI dispatch)
- `src/loop_runner.rs:56-205` — Autonomous loop implementation
- `crates/rho-core/src/agent_loop.rs:12-22` — AgentLoopConfig struct
- `crates/rho-core/src/types.rs:100-137` — AgentEvent enum
- `crates/rho-core/src/config.rs:17-52` — Config file discovery chain
- `crates/rho-core/src/compaction.rs:82-121` — Compaction strategy
- `crates/rho-core/src/commands.rs:11-57` — Built-in command definitions
- `crates/rho-tools/src/task.rs:34-165` — Subagent tool implementation
- `crates/rho-hashline/src/apply.rs` — 907 lines of edit application logic
- `crates/rho-hashline/src/heuristics.rs` — 676 lines of fuzzy matching

## Related Research

- `thoughts/shared/research/2026-02-14-rho-architecture-extensions.md` — Future architecture plans
- `thoughts/shared/research/2026-02-14-rho-feature-comparison.md` — Feature matrix vs Claude Code, OpenCode, HumanLayer

## Open Questions

1. Should `rho-lib` and `rho-cli` be deleted now or held for future use?
2. Should the 5 uncommitted files be committed as one atomic commit or split?
3. What's the next priority: MCP support, multi-turn CLI, or permission model?
