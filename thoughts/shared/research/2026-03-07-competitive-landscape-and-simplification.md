# Competitive Landscape & Radical Simplification Roadmap

**Date**: 2026-03-07
**Purpose**: Deep dive into what rho has, what the competition does, and concrete improvements that keep things radically simple.

---

## Part 1: What Rho Has Today

### The Good

**Rust-native agent loop** — ~15,700 lines across a 9-crate workspace. The core loop in `rho-core/src/agent_loop.rs` is clean: stream LLM response → extract tool calls → execute → loop until done. Event-driven via mpsc channels. This is solid.

**Hashline editing** (`rho-hashline`) — xxHash32 checksums per line enable drift-resistant editing. When the file has changed since the model last read it, hashlines let the edit tool relocate the target region heuristically. This is genuinely better than string-match replacement (what Claude Code and most agents use). It's a real differentiator.

**9 tools**: read, write, edit, bash, grep, find, task (subagent), web_fetch, web_search. All implement the `AgentTool` trait with async + cancellation support.

**Native GUI** (Iced) + CLI entry point. Session persistence via SQLite. Ralph autonomous loops for multi-iteration workflows.

**Provider flexibility** — Anthropic + OpenAI-compatible streaming via SSE.

### The Rough Edges

1. **Too many crates for the current scale.** 9 crates for 15K lines means high coordination cost per change. `rho-lib` and `rho-cli` are stubs. `anthropic-auth` is 400 lines.

2. **No context compaction.** `transform_messages` exists as an extension point but isn't implemented. For long sessions, this will hit the wall.

3. **No memory system.** No cross-session learning. Every session starts cold.

4. **System prompt is large** (~124 lines base + skills + memories + commands). Each tool has detailed descriptions baked in. More tools = more tokens consumed before the model even starts working.

5. **SCUD integration incomplete.** Referenced in CLAUDE.md but the task tool is generic subagent invocation, not SCUD-aware.

6. **No sandbox.** Bash tool has PTY support and output truncation but no filesystem/network isolation.

7. **No edit format optimization.** One edit format (hashline or text-replace). No adaptation based on model capabilities.

8. **GUI is partially complete.** Functional but rough.

---

## Part 2: What the Competition Does

### Pi (Mario Zechner) — The Radical Minimalist

**Philosophy**: YAGNI as religion. "If I don't need it, it won't be built."

| Aspect | Pi | Rho |
|--------|-----|-----|
| Tools | **4** (read, write, edit, bash) | 9 |
| System prompt | **~300 words** | ~124 lines + tool descriptions |
| Architecture | 4 npm workspaces | 9 Rust crates |
| Session model | Append-only JSONL with tree branching | SQLite |
| Extensibility | Hot-reloadable TS extensions | Compiled Rust |

**Key insight**: Pi beats feature-rich agents on Terminal-Bench with 4 tools. The model already knows CLIs — `rg` for grep, `gh` for GitHub, `find` for file search. Building tools for things the model can already do via bash is wasted tokens.

**Self-extending**: Pi's agent can write its own TypeScript extensions, hot-reload them, and test them in the same session. The scaffold builds itself.

**Session trees**: Sessions are deterministic tree structures. You can branch off for a side-quest and return to the main context without pollution. Simple and powerful.

### OpenAI Codex CLI — The Enterprise Minimalist

**Philosophy**: Sandbox-first. Single native binary. Zero runtime dependencies.

| Aspect | Codex CLI | Rho |
|--------|-----------|-----|
| Language | Rust (96%) | Rust |
| Sandbox | **OS-kernel level** (Landlock + seccomp + namespaces on Linux, Seatbelt on macOS) | None |
| Context mgmt | **Encrypted compaction** — opaque blob preserves model's latent state | Not implemented |
| Config standard | **AGENTS.md** (adopted by 8+ tools) | CLAUDE.md (proprietary) |
| Tools | Shell + ApplyPatch + MCP | 9 built-in |
| Approval model | Session-scoped trust lists | None |

**Key insights**:

1. **Encrypted compaction** is clever. Instead of summarizing old context (lossy), the Responses API produces an opaque encrypted blob that preserves the model's latent understanding. Much better than naive summarization.

2. **AGENTS.md as ecosystem standard.** Now supported by Codex, Cursor, Copilot, Amp, Gemini, Windsurf, Cline, Aider. Rho should adopt this.

3. **Skills as lazy-loaded metadata.** Only name/description injected into context. Full body stays on disk until invoked. Keeps token budget lean.

4. **Tab vs Enter (Steer Mode).** Two ways to inject mid-turn guidance without fragmenting context.

5. **`codex sandbox` debug command** — test sandbox behavior directly. Great DX.

### Devin — The Autonomous Engineer

**Philosophy**: Async junior engineer you talk to on Slack.

| Aspect | Devin | Rho |
|--------|-------|-----|
| Execution model | **Cloud sandbox** (own shell, editor, browser) | Local |
| Interaction | **Async via Slack** | Interactive CLI/GUI |
| Planning | **Adaptive multi-step plans** with human checkpoints | None |
| Parallelism | Multiple Devin instances on different tasks | Single session |
| Sweet spot | 4-8 hour junior engineer tasks | Interactive sessions |

**Key insights**:

1. **Planning matters.** Devin produces step-by-step plans before coding. Users can edit/reorder/approve. With Devin 2.0, "Interactive Planning" lets you collaboratively scope.

2. **Autonomy has limits.** Answer.AI found Devin would "spend days pursuing impossible solutions rather than recognizing fundamental blockers." Cognition's own guidance: "If it's going in circles, discontinue the conversation."

3. **RL fine-tuning was transformative.** 2x improvement in task completion, 4x in speed after fine-tuning on their execution traces.

4. **The right UX is "colleague on Slack", not "tool in terminal"** — for certain workloads. Rho doesn't need to be Devin, but async task execution is worth considering.

### OpenCode — The Provider-Agnostic Platform

**Philosophy**: Client/server separation. The TUI is just one frontend.

| Aspect | OpenCode | Rho |
|--------|----------|-----|
| Architecture | **HTTP server + SSE clients** | Monolithic binary |
| Stack | TypeScript (Bun) + Go TUI + SolidJS web | Rust + Iced GUI |
| Providers | **75+ LLM providers** via Vercel AI SDK | Anthropic + OpenAI-compat |
| Tools | 14 built-in + custom + plugins + MCP | 9 built-in |
| Context mgmt | **Auto-compaction at 95% window + tool output pruning** | Not implemented |
| Config | AGENTS.md + CLAUDE.md fallback | CLAUDE.md |

**Key insights**:

1. **Client/server split** enables remote execution (run on a powerful machine, drive from phone/browser). Elegant.

2. **Provider-specific prompts.** Claude gets `anthropic.txt`, GPT gets `beast.txt`, Gemini gets `gemini.txt`. Different models need different system prompts.

3. **Tool output pruning** — walks backwards through recent tool calls, protects ~40K tokens of recent output, replaces older output with placeholders. Simple and effective.

4. **Event bus** (typed pub/sub) fully decouples agent loop from UI/connection management.

5. **Instance scoping** — request-scoped dependency injection without a framework. Clean pattern.

### Aider — The Context Engineer

**Key insight: Repository Map.** Tree-sitter extracts function/class signatures → NetworkX dependency graph → PageRank finds most relevant code → fits into configurable token budget. This is the smartest context selection approach.

**Edit format research.** Different models work best with different edit strategies (whole-file, search/replace, unified diffs). Aider benchmarks and selects the best format per model. High-leverage optimization.

**Architect/Editor split.** One LLM reasons about the solution, another translates to edits. SOTA results on Aider's benchmark.

### SWE-agent / Mini-SWE-agent — The Research Baseline

**Mini-SWE-agent: 100 lines of Python, 74% on SWE-bench.**

This is the strongest possible argument for radical simplicity:
- **Bash-only tools.** No tool-calling API. The model writes bash commands in its responses.
- **Stateless execution.** Every command via `subprocess.run`. Independent. Trivially swappable to Docker.
- **Linear, append-only history.** The trajectory IS the message history. No branching, no compression.

**SWE-agent's ACI concept**: Treat the LLM as an end user. Design the tool interface for the model's strengths and limitations — compact actions, structured feedback, clear documentation.

---

## Part 3: Patterns That Matter

Distilling across all agents, these patterns consistently produce results:

### 1. Fewer Tools is Better

| Agent | Tools | Benchmark |
|-------|-------|-----------|
| Mini-SWE-agent | 1 (bash) | 74% SWE-bench |
| Pi | 4 | Beats Terminal-Bench |
| Codex CLI | 2 (shell + patch) + MCP | Production |
| Rho | 9 | N/A |

Every tool is tokens in the system prompt. Every tool is a decision the model must make. **The model already knows bash.** It can `rg` for grep, `find` for glob, `cat` for read. The question is: which built-in tools earn their token cost vs. just using bash?

**Read and Edit earn their keep** — structured output (hashlines) and drift recovery can't be done via bash. **Grep, find, web_search, web_fetch** are debatable — the model can use CLI equivalents.

### 2. Context Management is the Hard Problem

Every successful agent has a strategy here:
- **Codex**: Encrypted compaction (preserves latent state)
- **OpenCode**: Auto-compact at 95% + tool output pruning
- **Aider**: PageRank repo map (proactive context selection)
- **Pi**: Small system prompt + manual control
- **Rho**: Nothing yet

This is the single biggest gap in rho.

### 3. The Scaffold Should Get Out of the Way

Mini-SWE-agent's 100 lines. Pi's 4 tools. The model IS the agent. The scaffold's job is:
1. Pass messages
2. Execute tools
3. Manage context window
4. Get out of the way

Everything else is overhead unless proven otherwise.

### 4. Git is Your Undo System

Don't build custom undo/redo. Auto-commit after changes. Git already handles branching, reverting, and history.

### 5. Edit Format Matters

Aider's research: the edit format you choose interacts strongly with model capabilities. Hashline is good for drift detection but may not be the optimal format for all models. Worth benchmarking.

### 6. Extensibility > Features

Pi's approach: ship primitives, let users compose. A package system for distributing extensions beats adding every feature to core.

---

## Part 4: Concrete Improvements for Rho

Ordered by impact and simplicity. Each tagged with effort estimate.

### Tier 1: High Impact, Keep It Simple

#### 1.1 Implement Context Compaction [Medium effort]

The most critical gap. Without this, long sessions will fail.

**Approach**: When token count exceeds 80% of window:
1. Take all messages except the last N tool exchanges
2. Send to a fast model with "Summarize the key context, decisions, and current state"
3. Replace old messages with the summary as a single system message
4. Keep working

OpenCode's approach (compact at 95%, protect ~40K recent tokens) is a good starting point. Don't try encrypted compaction — that's an API-level feature.

#### 1.2 Reduce to 5 Core Tools [Small effort]

Current 9 → proposed 5:

| Keep | Why |
|------|-----|
| `read` | Hashline output is a real advantage |
| `write` | File creation needs structured handling |
| `edit` | Hashline drift recovery is genuinely better |
| `bash` | Universal tool — subsumes grep, find, web operations |
| `task` | Subagent invocation is valuable for parallel work |

**Drop**: `grep`, `find`, `web_fetch`, `web_search`. The model can:
- `rg` instead of grep tool (it already knows ripgrep)
- `find` or `fd` via bash
- `curl` + process via bash for web
- `ddgr` or similar for web search via bash

This shrinks the system prompt significantly and frees token budget for actual work.

**Counterargument**: Structured tool output (hashlines in grep, .gitignore respect in find) adds value. **Compromise**: Keep grep and find as "power tools" available but not in default system prompt. Load them on demand like Codex's lazy skills.

#### 1.3 Adopt AGENTS.md Convention [Small effort]

Read `AGENTS.md` in addition to (or instead of) `CLAUDE.md`. This aligns with the emerging ecosystem standard (Codex, Cursor, Copilot, Amp, Gemini, Windsurf, Cline, Aider all support it). Zero-cost interop.

#### 1.4 Lazy-Load Tool/Skill Descriptions [Small effort]

Currently all 9 tool descriptions are in the system prompt. Instead:
- Include a brief one-liner per tool (name + 10-word description)
- Full description loaded only when the tool is first invoked
- Skills: only name/description in context, full body on disk

This could recover 2-3K tokens in the system prompt.

### Tier 2: Medium Impact, Medium Effort

#### 2.1 Add Planning Mode [Medium effort]

Before executing, produce a step-by-step plan. User can approve, edit, or reject. This is what Devin, Codex, and OpenCode all do. Implementation:
- On task start, add a "plan" phase that asks the model to outline steps
- Display to user for approval
- Execute steps sequentially with progress tracking
- Adaptive: update plan as new information surfaces

#### 2.2 Consolidate Crates [Medium effort]

9 crates → 4:

| New Crate | Absorbs | Rationale |
|-----------|---------|-----------|
| `rho-core` | + `rho-session` + `rho-hashline` | Core types, loop, persistence, hashline are tightly coupled |
| `rho-tools` | (unchanged) | Tool implementations |
| `rho-provider` | + `anthropic-auth` | Provider + auth are one concern |
| `rho-gui` | (unchanged) | GUI is legitimately separate |

Delete `rho-lib` and `rho-cli` stubs. Move `main.rs` and `loop_runner.rs` into `rho-core` or a thin binary crate.

#### 2.3 Session Branching [Medium effort]

Pi's tree-structured sessions are elegant. Before a risky operation:
1. Snapshot current conversation state
2. Try the approach
3. If it fails, rewind to snapshot and try differently

Implementation: store sessions as append-only logs with branch points. SQLite already supports this with a parent_id column.

#### 2.4 Auto-Commit on Successful Edit [Small effort]

After each successful write/edit, auto-commit with a descriptive message. Git becomes the undo system. This is what Aider does and it's beloved.

### Tier 3: Future Considerations

#### 3.1 Provider-Specific Prompts

Different models need different system prompts. Claude is verbose, GPT is more structured, Gemini has its quirks. OpenCode maintains separate prompt files per provider family. When rho supports multiple providers seriously, this will matter.

#### 3.2 Repository Map (Aider-style)

Tree-sitter parsing → dependency graph → PageRank for relevant files. This is the most sophisticated context selection approach. High effort but high reward for large codebases.

#### 3.3 Sandboxing

Codex's approach: OS-kernel level isolation (Landlock + seccomp on Linux). This is table stakes for production use. Medium-high effort but critical for trust.

#### 3.4 Client/Server Split

OpenCode's approach: core as HTTP server, TUI/GUI/web as clients. Enables remote operation. High effort, high reward for certain use cases.

#### 3.5 Hot-Reloadable Extensions

Pi's approach: the agent writes TypeScript extensions, hot-reloads, and tests them in the same session. For a Rust agent, this could mean:
- WASM plugins
- Lua/Rhai scripting
- Or just: the agent writes a bash script and calls it

The simplest version is "the agent writes bash scripts and adds them to PATH."

---

## Part 5: The Radically Simple Target Architecture

If we rebuilt rho from first principles with everything we've learned:

```
rho (single binary, ~5K lines Rust)
│
├── Core Loop (500 lines)
│   ├── Stream LLM response
│   ├── Extract + execute tool calls
│   ├── Context compaction when window fills
│   └── Loop until done
│
├── 5 Tools
│   ├── read   — file → hashline output
│   ├── write  — create/overwrite file
│   ├── edit   — hashline-aware editing with drift recovery
│   ├── bash   — universal tool (subsumes grep, find, web, etc.)
│   └── agent  — spawn sub-agent for parallel work
│
├── Context Management
│   ├── Token counting
│   ├── Auto-compaction (summarize old context)
│   └── Tool output pruning (protect recent, trim old)
│
├── Session Persistence
│   ├── Append-only JSONL (simple, debuggable)
│   ├── Branch points for speculative execution
│   └── Cross-session memory (key facts file)
│
├── System Prompt (~500 tokens)
│   ├── Brief tool descriptions (one-liners)
│   ├── AGENTS.md / CLAUDE.md instructions
│   └── Current date/environment
│
└── Configuration
    ├── AGENTS.md (project-level)
    ├── ~/.config/rho/config.toml (global)
    └── Provider settings
```

**What's NOT here**: GUI (separate binary if needed), 9 tools, large system prompts, 9 crates, MCP, skills system, memories system, commands system.

**Philosophy**: The model is the agent. The scaffold passes messages, executes 5 tools, manages context, and gets out of the way. Everything else is overhead until proven otherwise by benchmarks.

---

## Part 6: What to Do Monday Morning

1. **Implement context compaction.** This unblocks long sessions. Start with OpenCode's approach: compact at 80% window, protect recent 40K tokens, summarize the rest.

2. **Trim tools to 5.** Drop grep/find/web_fetch/web_search from default set. Let the model use bash for those. Measure if task completion drops (it probably won't).

3. **Shrink the system prompt.** Move from detailed tool descriptions to one-liners. Lazy-load full descriptions on first use.

4. **Read AGENTS.md.** One-line change to check for `AGENTS.md` alongside `CLAUDE.md`.

5. **Auto-commit after edits.** `git add <file> && git commit -m "<description>"` after each successful edit. Configurable, off by default.

These 5 changes keep rho radically simple while closing the biggest gaps with the competition.
