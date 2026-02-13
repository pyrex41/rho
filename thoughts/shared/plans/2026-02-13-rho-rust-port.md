# Rho: PiCodingAgent Rust Port — Implementation Plan

## Overview

Port the PiCodingAgent (a TypeScript/Bun coding agent) to Rust as a Cargo workspace of 7 crates. The agent has a ~400 LOC core loop, 6 tools (Read, Edit, Write, Bash, Grep, Find), hashline-based file editing with xxHash32 integrity checks, SSE streaming for the Anthropic API, and Claude Code OAuth support. A future GUI crate (Iced) is planned but out of scope for v1.

## Current State Analysis

The `reference/` directory contains the complete TypeScript source for every component:
- `reference/upstream/agent-loop.ts` — Core loop (~418 lines)
- `reference/upstream/agent.ts` — Agent class wrapper (~537 lines)
- `reference/upstream/anthropic-provider.ts` — Anthropic API + SSE (~852 lines)
- `reference/upstream/ai-types.ts` + `agent-types.ts` — Type system
- `reference/upstream/event-stream.ts` — Async event stream (~88 lines)
- `reference/upstream/claude-code-auth/` — Full OAuth flow
- `reference/fork/hashline.ts` — Hashline system (~991 lines)
- `reference/fork/{edit,read,write,bash,grep,find}-tool.ts` — All 6 tools
- `reference/FAST-TOOLS-AND-HASHLINE.md` — Rust-specific crate selections + hashline spec

The `rho/` repo is empty (no commits). Everything starts from scratch.

## Desired End State

A single `cargo build` produces two binaries:
1. **`rho`** — The coding agent CLI. Accepts a prompt, streams Claude's response, executes tool calls, loops until done.
2. **`anthropic-auth`** — Standalone OAuth CLI. `anthropic-auth login` → browser OAuth → cached token. `anthropic-auth token` → prints current token.

The agent supports:
- All 6 tools with hashline editing
- Anthropic API with both API key and OAuth authentication
- Claude Code identity headers for subscription-based access
- SSE streaming with text, thinking, and tool_use content blocks
- Configurable tool backends via `tools.toml`
- Steering (mid-run user interrupts) and follow-up messages

### Verification:
```bash
# Build both binaries
cargo build --release

# API key mode
ANTHROPIC_API_KEY=sk-ant-api-... cargo run -- "Write hello world to /tmp/test.txt"
# Should create the file and exit

# OAuth mode
cargo run --bin anthropic-auth -- login    # Browser opens, token cached
cargo run -- "Read /tmp/test.txt"          # Uses cached OAuth token

# Hashline editing
cargo run -- "Read src/main.rs, then add a comment on line 1"
# Should use hashline anchors for the edit

# Configurable backends
cat ~/.config/rho/tools.toml   # Shows auto-detected tool backends
```

## What We're NOT Doing

- **GUI (`rho-gui`)**: Iced-based conversation graph viewer is v2. The workspace structure includes the crate stub but no implementation.
- **Multi-provider support**: Anthropic-only for v1. No OpenAI, Google, Bedrock.
- **LSP integration**: The reference edit tool has format-on-write and diagnostics via LSP. Skipped for v1.
- **Image support in Read tool**: Base64 image reading is skipped for v1.
- **Document conversion**: The reference uses `markitdown` for PDF/DOCX. Skipped.
- **Internal URL routing**: `agent://` and `skill://` URL schemes from the reference. Skipped.
- **MCP, plugins, sub-agents**: Not in the reference implementation we're porting.
- **Context window management**: The `transformContext()` hook is wired but the actual pruning/compression logic is v2.

## Implementation Approach

Vertical slice: get a minimal end-to-end agent working first (Phase 2), then layer in hashline (Phase 4), remaining tools (Phase 5), and OAuth (Phase 6). Each phase produces a testable, working state.

---

## Phase 1: Workspace + Core Types

### Overview
Set up the Cargo workspace with all 7 crate stubs and define the core type system that every other crate depends on.

### Changes Required:

#### 1. Workspace Root

**File**: `Cargo.toml`

```toml
[workspace]
resolver = "2"
members = [
    "crates/rho-core",
    "crates/rho-hashline",
    "crates/rho-tools",
    "crates/rho-provider",
    "crates/anthropic-auth",
    "crates/rho-gui",
]

[workspace.dependencies]
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
reqwest = { version = "0.12", features = ["stream", "json"] }
clap = { version = "4", features = ["derive"] }
thiserror = "2"
anyhow = "1"
tracing = "0.1"
tracing-subscriber = "0.3"
```

#### 2. Core Types (`rho-core`)

**File**: `crates/rho-core/src/types.rs`

The heart of the type system. Every other crate imports from here.

```rust
use serde::{Deserialize, Serialize};

// === Content Types ===

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Content {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "thinking")]
    Thinking { thinking: String },
    #[serde(rename = "image")]
    Image { data: String, mime_type: String },
    #[serde(rename = "toolCall")]
    ToolCall {
        id: String,
        name: String,
        arguments: serde_json::Value,
    },
}

// === Messages ===

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "role")]
pub enum Message {
    #[serde(rename = "user")]
    User {
        content: UserContent,
        timestamp: u64,
    },
    #[serde(rename = "assistant")]
    Assistant {
        content: Vec<Content>,
        model: String,
        usage: Usage,
        stop_reason: StopReason,
        timestamp: u64,
    },
    #[serde(rename = "toolResult")]
    ToolResult {
        tool_call_id: String,
        tool_name: String,
        content: Vec<Content>,
        is_error: bool,
        timestamp: u64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum UserContent {
    Text(String),
    Blocks(Vec<Content>),
}

// === Usage / Stop Reason ===

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum StopReason {
    Stop,
    Length,
    ToolUse,
    Error,
    Aborted,
}

// === Model ===

#[derive(Debug, Clone)]
pub struct Model {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub base_url: String,
    pub reasoning: bool,
    pub context_window: usize,
    pub max_tokens: usize,
}

// === Tool Definition ===

pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value, // JSON Schema
}

// === Agent Event (the union type that drives everything) ===

#[derive(Debug, Clone)]
pub enum AgentEvent {
    AgentStart,
    AgentEnd { messages: Vec<Message> },
    TurnStart,
    TurnEnd {
        message: Message,
        tool_results: Vec<Message>,
    },
    MessageStart { message: Message },
    MessageUpdate {
        message: Message,
        event: AssistantStreamEvent,
    },
    MessageEnd { message: Message },
    ToolExecutionStart {
        tool_call_id: String,
        tool_name: String,
        args: serde_json::Value,
    },
    ToolExecutionUpdate {
        tool_call_id: String,
        tool_name: String,
        partial_result: ToolResult,
    },
    ToolExecutionEnd {
        tool_call_id: String,
        tool_name: String,
        result: ToolResult,
        is_error: bool,
    },
}

// === Tool Result ===

#[derive(Debug, Clone)]
pub struct ToolResult {
    pub content: Vec<Content>,
    pub details: serde_json::Value,
}

// === SSE Stream Events ===

#[derive(Debug, Clone)]
pub enum AssistantStreamEvent {
    Start,
    TextStart { index: usize },
    TextDelta { index: usize, delta: String },
    TextEnd { index: usize, content: String },
    ThinkingStart { index: usize },
    ThinkingDelta { index: usize, delta: String },
    ThinkingEnd { index: usize, content: String },
    ToolCallStart { index: usize },
    ToolCallDelta { index: usize, delta: String },
    ToolCallEnd { index: usize, tool_call: Content },
    Done { stop_reason: StopReason },
    Error { stop_reason: StopReason },
}

// === Thinking Level ===

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub enum ThinkingLevel {
    Off,
    Minimal,
    Low,
    #[default]
    Medium,
    High,
}
```

#### 3. Event Stream (`rho-core`)

**File**: `crates/rho-core/src/event_stream.rs`

Port of the ~88 line TypeScript `EventStream`. In Rust, use `tokio::sync::mpsc` + `tokio::sync::oneshot`.

```rust
use tokio::sync::{mpsc, oneshot};

pub struct EventStream<T: Clone + Send + 'static, R: Send + 'static> {
    tx: mpsc::UnboundedSender<T>,
    rx: mpsc::UnboundedReceiver<T>,
    result_tx: Option<oneshot::Sender<R>>,
    result_rx: oneshot::Receiver<R>,
    is_complete: Box<dyn Fn(&T) -> bool + Send>,
    extract_result: Box<dyn Fn(&T) -> Option<R> + Send>,
}

// Methods: push(), end(), next() (async), result() (async)
// Consumer side implements Stream trait for `for await` equivalent
```

#### 4. AgentTool Trait (`rho-core`)

**File**: `crates/rho-core/src/tool.rs`

```rust
use async_trait::async_trait;

#[async_trait]
pub trait AgentTool: Send + Sync {
    fn name(&self) -> &str;
    fn label(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters_schema(&self) -> serde_json::Value;

    async fn execute(
        &self,
        tool_call_id: &str,
        params: serde_json::Value,
        cancel: tokio::sync::CancellationToken,
    ) -> Result<ToolResult, ToolError>;
}

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("{0}")]
    ExecutionError(String),
    #[error("Tool cancelled")]
    Cancelled,
}
```

#### 5. Crate Stubs

Create `Cargo.toml` + `src/lib.rs` for each crate:
- `rho-core`: types, event_stream, tool trait, agent_loop (placeholder)
- `rho-hashline`: empty `lib.rs`
- `rho-tools`: empty, depends on `rho-core`, `rho-hashline`
- `rho-provider`: empty, depends on `rho-core`, `anthropic-auth`
- `anthropic-auth`: empty lib + `src/bin/main.rs` stub
- `rho-gui`: empty, depends on `rho-core`

Also create `src/main.rs` for the `rho` CLI binary.

### Success Criteria:

#### Automated Verification:
- [ ] `cargo build` succeeds for the entire workspace
- [ ] `cargo test` passes (even if no tests yet, no compile errors)
- [ ] `cargo clippy` clean
- [ ] All types compile and are importable across crates

---

## Phase 2: Minimal End-to-End Agent

### Overview
Wire up the minimal path: API key auth → SSE streaming → agent loop → Write tool → CLI output. The goal is `cargo run -- "Write hello to /tmp/test.txt"` working end-to-end.

### Changes Required:

#### 1. API Key Auth (`anthropic-auth`)

**File**: `crates/anthropic-auth/src/lib.rs`

Minimal — just reads `ANTHROPIC_API_KEY` env var. OAuth comes in Phase 6.

```rust
pub async fn get_token() -> Result<String> {
    // Priority:
    // 1. ANTHROPIC_API_KEY env var
    // 2. Error: no auth configured
    std::env::var("ANTHROPIC_API_KEY")
        .map_err(|_| anyhow!("Set ANTHROPIC_API_KEY or run `anthropic-auth login`"))
}

pub fn is_oauth_token(token: &str) -> bool {
    token.starts_with("sk-ant-oat")
}
```

#### 2. Anthropic Provider (`rho-provider`)

**File**: `crates/rho-provider/src/lib.rs`

The SSE streaming implementation. Key responsibilities:
- Build the Messages API request payload
- Stream SSE events, parse `message_start`, `content_block_start/delta/stop`, `message_delta`
- Emit `AssistantStreamEvent` variants
- Handle auth branching (API key vs OAuth headers)

```rust
pub struct AnthropicProvider {
    client: reqwest::Client,
    base_url: String,
}

impl AnthropicProvider {
    pub fn new(api_key: &str) -> Self { ... }

    pub async fn stream(
        &self,
        model: &Model,
        system_prompt: &str,
        messages: &[Message],      // LLM-format messages
        tools: &[ToolDef],
        options: &StreamOptions,
    ) -> Result<EventStream<AssistantStreamEvent, Message>> { ... }
}
```

**SSE parsing**: Use `reqwest` to get a byte stream, split on `\n\n`, parse `event:` and `data:` fields. Events from the Anthropic API:

```
event: message_start       → data has message.id, model, usage
event: content_block_start → data has index + type (text/thinking/tool_use)
event: content_block_delta → data has index + delta (text_delta/thinking_delta/input_json_delta)
event: content_block_stop  → data has index
event: message_delta       → data has stop_reason, usage
event: message_stop        → stream complete
```

Tool call arguments arrive as incremental JSON chunks via `input_json_delta`. Accumulate into a `String`, then parse with `serde_json` when `content_block_stop` fires.

#### 3. Write Tool (`rho-tools`)

**File**: `crates/rho-tools/src/write.rs`

The simplest tool — validates the agent loop wiring.

```rust
pub struct WriteTool;

#[async_trait]
impl AgentTool for WriteTool {
    fn name(&self) -> &str { "Write" }
    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path to write" },
                "content": { "type": "string", "description": "Content to write" }
            },
            "required": ["path", "content"]
        })
    }

    async fn execute(&self, _id: &str, params: Value, _cancel: CancellationToken)
        -> Result<ToolResult, ToolError>
    {
        let path = params["path"].as_str().ok_or(...)?;
        let content = params["content"].as_str().ok_or(...)?;
        // Create parent dirs, write file
        tokio::fs::create_dir_all(Path::new(path).parent().unwrap_or(Path::new("."))).await?;
        tokio::fs::write(path, content).await?;
        Ok(ToolResult {
            content: vec![Content::Text { text: format!("Wrote {} bytes to {}", content.len(), path) }],
            details: json!({}),
        })
    }
}
```

#### 4. Agent Loop (`rho-core`)

**File**: `crates/rho-core/src/agent_loop.rs`

The core loop. Port of `reference/upstream/agent-loop.ts`.

```rust
pub struct AgentLoopConfig {
    pub model: Model,
    pub api_key: String,
    pub system_prompt: String,
    pub tools: Vec<Box<dyn AgentTool>>,
    pub convert_to_llm: Box<dyn Fn(&[Message]) -> Vec<Message> + Send>,
    pub transform_context: Option<Box<dyn Fn(&[Message]) -> Vec<Message> + Send>>,
    pub get_steering_messages: Option<Box<dyn Fn() -> Vec<Message> + Send>>,
    pub get_follow_up_messages: Option<Box<dyn Fn() -> Vec<Message> + Send>>,
}

pub fn agent_loop(
    prompts: Vec<Message>,
    context: Vec<Message>,
    config: AgentLoopConfig,
    cancel: CancellationToken,
) -> EventStream<AgentEvent, Vec<Message>> {
    let stream = EventStream::new(...);

    tokio::spawn(async move {
        let mut messages = context;
        messages.extend(prompts);

        stream.push(AgentEvent::AgentStart);
        stream.push(AgentEvent::TurnStart);

        run_loop(&mut messages, &config, &cancel, &stream).await;
    });

    stream
}

async fn run_loop(...) {
    // Outer loop: follow-up messages
    loop {
        // Inner loop: tool calls + steering
        loop {
            // 1. Stream assistant response
            let assistant_msg = stream_assistant_response(...).await;

            // 2. Check for error/abort
            if matches!(assistant_msg.stop_reason(), StopReason::Error | StopReason::Aborted) {
                break;
            }

            // 3. Extract tool calls
            let tool_calls = extract_tool_calls(&assistant_msg);
            if tool_calls.is_empty() { break; }

            // 4. Execute tool calls sequentially
            for tool_call in &tool_calls {
                let tool = find_tool(&config.tools, &tool_call.name);
                let result = tool.execute(tool_call.id, tool_call.arguments, cancel.clone()).await;
                // Push tool result message
                // Check for steering messages between tool calls
            }
        }

        // Check for follow-up messages
        let follow_ups = config.get_follow_up_messages.as_ref().map(|f| f()).unwrap_or_default();
        if follow_ups.is_empty() { break; }
    }

    stream.push(AgentEvent::AgentEnd { messages });
}
```

#### 5. CLI Entry Point

**File**: `src/main.rs`

```rust
#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();  // clap
    let api_key = anthropic_auth::get_token().await?;

    let tools: Vec<Box<dyn AgentTool>> = vec![
        Box::new(WriteTool),
    ];

    let prompt = Message::User {
        content: UserContent::Text(args.prompt),
        timestamp: now_ms(),
    };

    let stream = agent_loop(vec![prompt], vec![], config, cancel);

    // Consume events, print text deltas to stdout
    while let Some(event) = stream.next().await {
        match event {
            AgentEvent::MessageUpdate { event: AssistantStreamEvent::TextDelta { delta, .. }, .. } => {
                print!("{}", delta);
                std::io::stdout().flush()?;
            }
            AgentEvent::ToolExecutionEnd { tool_name, result, .. } => {
                eprintln!("[tool:{}] {}", tool_name, result.content[0].text());
            }
            // ... other events
            _ => {}
        }
    }
    Ok(())
}
```

### Success Criteria:

#### Automated Verification:
- [ ] `cargo build` succeeds
- [ ] `cargo test` passes
- [ ] `cargo clippy` clean

#### Manual Verification:
- [ ] `ANTHROPIC_API_KEY=... cargo run -- "Write 'hello world' to /tmp/rho-test.txt"` creates the file
- [ ] Assistant text streams to stdout in real-time (not buffered)
- [ ] Tool call execution is visible in output
- [ ] Agent exits cleanly after completing the task

**Implementation Note**: After completing this phase, pause for manual verification that the end-to-end flow works before proceeding.

---

## Phase 3: Read + Bash Tools

### Overview
Add Read and Bash tools so the agent can inspect files and run commands. Read uses basic line-number formatting (not hashline yet — that comes in Phase 4).

### Changes Required:

#### 1. Read Tool

**File**: `crates/rho-tools/src/read.rs`

Key behaviors:
- Read file contents with line numbers (format: `N|content` for now, upgraded to hashline in Phase 5)
- Support `offset` and `limit` parameters for partial reads
- Directory listing with modification times when path is a directory
- Fuzzy path suggestions on file-not-found

```rust
pub struct ReadTool;

// Parameters: path, offset (optional), limit (optional)
// Output: numbered lines, or directory listing, or error with suggestions
```

#### 2. Bash Tool

**File**: `crates/rho-tools/src/bash.rs`

The most complex tool in terms of system interaction. Uses `portable-pty` for PTY support.

Key behaviors:
- Execute command in a PTY (so interactive programs work)
- Configurable timeout (default 300s, max 3600s)
- Stream output chunks during execution (via `onUpdate` callback)
- Capture and return final output (head/tail truncation for large output)
- Working directory support

```rust
pub struct BashTool {
    working_dir: PathBuf,
}

// Parameters: command (string), timeout (int, optional)
// Execution: spawn PTY process, read output with timeout, return result
// Output truncation: if output > 100KB, return first 10KB + last 10KB with "[...truncated...]"
```

**PTY approach**: Use `portable-pty` crate to create a pseudo-terminal pair, spawn the command in it, and read output asynchronously with a timeout.

```rust
use portable_pty::{CommandBuilder, PtySize, native_pty_system};

async fn run_bash(command: &str, timeout_secs: u64, cwd: &Path) -> Result<String> {
    let pty_system = native_pty_system();
    let pair = pty_system.openpty(PtySize { rows: 24, cols: 80, .. })?;
    let mut cmd = CommandBuilder::new("bash");
    cmd.args(["-c", command]);
    cmd.cwd(cwd);
    let child = pair.slave.spawn_command(cmd)?;
    let reader = pair.master.try_clone_reader()?;
    // Read with timeout using tokio::time::timeout
    // Kill child if timeout expires
}
```

### Success Criteria:

#### Automated Verification:
- [ ] `cargo build` succeeds
- [ ] `cargo test` — unit tests for Read (file reading, directory listing)
- [ ] `cargo test` — unit tests for Bash (command execution, timeout)

#### Manual Verification:
- [ ] `cargo run -- "Read src/main.rs"` shows file contents with line numbers
- [ ] `cargo run -- "Run ls -la in the current directory"` executes and returns output
- [ ] `cargo run -- "Read src/"` shows directory listing
- [ ] Bash tool respects timeout (test with `sleep 999` + low timeout)

**Implementation Note**: Pause for manual verification before proceeding.

---

## Phase 4: Hashline System (`rho-hashline`)

### Overview
Implement the complete hashline system: hash computation, display formatting, edit operations, the application algorithm with all 7 heuristic cleanups, and mismatch error formatting. This is the most complex component (~991 lines in TS).

### Changes Required:

#### 1. Hash Computation

**File**: `crates/rho-hashline/src/hash.rs`

```rust
use xxhash_rust::xxh32::xxh32;

/// Pre-computed hex lookup table for hash values 0-255
const HASH_TABLE: [&str; 256] = /* compile-time generate "00".."ff" */;

/// Compute the hashline hash for a line of text.
/// Strips all whitespace, xxHash32, mod 256, 2-char hex.
pub fn compute_line_hash(line: &str) -> &'static str {
    let normalized: String = line.chars().filter(|c| !c.is_whitespace()).collect();
    let hash = xxh32(normalized.as_bytes(), 0) as usize % 256;
    HASH_TABLE[hash]
}
```

The `HASH_TABLE` can be generated at compile time with a `const fn` or a build script. Alternatively, use `lazy_static` with a runtime-initialized array, though compile-time is preferred.

#### 2. Display Formatting

**File**: `crates/rho-hashline/src/format.rs`

```rust
/// Format file content as hashlines: "LINE:HASH|content"
pub fn format_hashlines(content: &str, start_line: usize) -> String {
    content.lines()
        .enumerate()
        .map(|(i, line)| {
            let num = start_line + i;
            let hash = compute_line_hash(line);
            format!("{}:{}|{}", num, hash, line)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Stream hashlines in chunks for large files
pub fn stream_hashlines(content: &str, start_line: usize, chunk_size: usize)
    -> impl Iterator<Item = String> + '_
{ ... }
```

#### 3. Line Reference Parsing

**File**: `crates/rho-hashline/src/parse.rs`

```rust
/// A parsed line reference: "42:a3"
#[derive(Debug, Clone)]
pub struct LineRef {
    pub line: usize,    // 1-indexed
    pub hash: String,   // 2-char hex
}

/// Parse a "LINE:HASH" string
pub fn parse_line_ref(s: &str) -> Result<LineRef> { ... }

/// Parse anchor from "LINE:HASH|content" format (strips content after pipe)
pub fn parse_anchor(s: &str) -> Result<LineRef> { ... }
```

#### 4. Edit Operations

**File**: `crates/rho-hashline/src/edit.rs`

```rust
/// The four edit operation types
#[derive(Debug, Clone)]
pub enum HashlineEdit {
    SetLine {
        anchor: LineRef,
        new_text: String,
    },
    ReplaceLines {
        start_anchor: LineRef,
        end_anchor: LineRef,
        new_text: String,
    },
    InsertAfter {
        anchor: LineRef,
        text: String,
    },
    Replace {
        old_text: String,
        new_text: String,
        all: bool,
    },
}

/// Parse a JSON value into a HashlineEdit
pub fn parse_edit(value: &serde_json::Value) -> Result<HashlineEdit> { ... }
```

#### 5. Application Algorithm

**File**: `crates/rho-hashline/src/apply.rs`

This is the critical path. The algorithm MUST:
1. Parse all edits
2. Pre-validate ALL hash references before ANY mutation
3. Hash relocation (auto-fix line drift)
4. Deduplicate identical edits
5. Sort bottom-up (highest line number first)
6. Apply with heuristic cleanup

```rust
pub fn apply_hashline_edits(content: &str, edits: &[HashlineEdit]) -> Result<String> {
    let mut lines: Vec<String> = content.lines().map(String::from).collect();
    let original = lines.clone();

    // Phase 1: Resolve all references
    let resolved = resolve_all_refs(&edits, &lines)?;
    // This validates every anchor, performing relocation if needed
    // Throws HashlineMismatchError if any ref is invalid

    // Phase 2: Deduplicate
    let deduped = deduplicate(resolved);

    // Phase 3: Sort bottom-up
    let mut sorted = deduped;
    sorted.sort_by(|a, b| b.sort_line().cmp(&a.sort_line()));

    // Phase 4: Apply
    for edit in &sorted {
        let cleaned = apply_heuristics(&edit.new_text, &original, edit.range());
        lines.splice(edit.range(), cleaned);
    }

    Ok(lines.join("\n"))
}
```

**Hash relocation** — when a hash at the given line doesn't match, but exists uniquely elsewhere:

```rust
fn relocate_ref(ref_: &LineRef, lines: &[String]) -> Result<LineRef> {
    // 1. Check if hash matches at the given line
    let actual_hash = compute_line_hash(&lines[ref_.line - 1]);
    if actual_hash == ref_.hash { return Ok(ref_.clone()); }

    // 2. Search for unique occurrence of this hash
    let matches: Vec<usize> = lines.iter().enumerate()
        .filter(|(_, l)| compute_line_hash(l) == ref_.hash)
        .map(|(i, _)| i + 1)
        .collect();

    match matches.len() {
        1 => Ok(LineRef { line: matches[0], hash: ref_.hash.clone() }),
        0 => Err(mismatch_error(...)),
        _ => Err(mismatch_error(...)),  // Ambiguous, can't relocate
    }
}
```

#### 6. Heuristic Cleanup

**File**: `crates/rho-hashline/src/heuristics.rs`

All 7 heuristics from the reference implementation:

```rust
/// Apply all heuristic cleanups to new_text before splicing
pub fn apply_heuristics(
    new_text: &str,
    original_lines: &[String],
    edit_range: Range<usize>,
) -> Vec<String> {
    let mut lines: Vec<String> = new_text.lines().map(String::from).collect();

    // 1. Strip hashline prefixes (model copies "42:a7|content")
    strip_hashline_prefixes(&mut lines);

    // 2. Strip diff "+" markers (model uses unified diff format)
    strip_diff_plus_markers(&mut lines);

    // 3. Boundary echo stripping (model copies context around edits)
    strip_boundary_echo(&mut lines, original_lines, &edit_range);

    // 4. Indentation restoration (model strips leading whitespace)
    restore_indentation(&mut lines, original_lines, &edit_range);

    // 5. Wrapped line restoration (model reflows lines)
    restore_wrapped_lines(&mut lines, original_lines);

    // 6. Merge detection (model merges adjacent lines)
    detect_merges(&mut lines, original_lines, &edit_range);

    // 7. Confusable hyphen normalization (Unicode dashes → ASCII)
    normalize_hyphens(&mut lines);

    lines
}
```

Each heuristic has specific activation criteria (e.g., "only fire when >= 50% of non-empty lines have the prefix") to avoid false positives. See `reference/FAST-TOOLS-AND-HASHLINE.md` for the complete specification.

#### 7. Mismatch Error

**File**: `crates/rho-hashline/src/error.rs`

```rust
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct HashlineMismatchError {
    pub message: String,
    pub mismatched_lines: Vec<usize>,
}

/// Format a mismatch error with >>> markers and context
pub fn format_mismatch_error(
    mismatches: &[(usize, &str, &str)],  // (line, expected_hash, actual_hash)
    lines: &[String],
    context: usize,  // lines of context around each mismatch (default: 2)
) -> HashlineMismatchError { ... }
```

Output format:
```
2 lines have changed since last read. Use the updated LINE:HASH references shown below (>>> marks changed lines).

    3:7f|  const x = 1;
    4:a2|  const y = 2;
>>> 5:e1|  const z = 3;
    6:b4|  return x + y;
```

### Success Criteria:

#### Automated Verification:
- [ ] `cargo test -p rho-hashline` — comprehensive unit tests:
  - Hash computation matches reference impl for known inputs
  - Format output matches expected `LINE:HASH|content` format
  - Each edit operation type parses and applies correctly
  - Hash relocation works (line drift scenario)
  - Mismatch error format is correct
  - All 7 heuristics activate correctly and don't false-positive
  - Deduplication works
  - Bottom-up sort order is correct
  - Pre-validation catches all invalid refs before mutation

---

## Phase 5: Full Tool Suite

### Overview
Upgrade Read to hashline output, implement Edit (hashline mode), Grep (ripgrep internals), Find (ignore + globset), and the configurable backend trait.

### Changes Required:

#### 1. Read Tool — Hashline Upgrade

**File**: `crates/rho-tools/src/read.rs`

Change output from `N|content` to `N:HASH|content` using `rho_hashline::format_hashlines()`.

#### 2. Edit Tool

**File**: `crates/rho-tools/src/edit.rs`

Two modes:
- **Hashline mode** (primary): Parse JSON edits array, call `rho_hashline::apply_hashline_edits()`
- **Replace mode** (fallback): Simple `old_text` → `new_text` fuzzy match using `nucleo-matcher`

```rust
pub struct EditTool;

// Parameters: path (string), edits (array of operations)
// 1. Read current file content
// 2. Parse edits JSON into HashlineEdit vec
// 3. Apply via rho_hashline::apply_hashline_edits()
// 4. Write result back to file
// 5. Return diff summary
```

The tool detects which mode to use based on the edits array content (if edits contain `set_line`/`replace_lines`/`insert_after` → hashline mode; if only `replace` → replace mode).

#### 3. Grep Tool

**File**: `crates/rho-tools/src/grep.rs`

Uses `grep-searcher` + `grep-regex` (ripgrep internals) with hashline-formatted output.

```rust
use grep_regex::RegexMatcherBuilder;
use grep_searcher::{Searcher, SearcherBuilder, Sink, SinkMatch, SinkContext};

pub struct GrepTool {
    working_dir: PathBuf,
}

// Parameters: pattern, path (optional), glob (optional), context_before,
//             context_after, case_sensitive, multiline, limit, offset
// Output format:
//   Match 1: src/main.rs:42
//       40:7f|  let config = Config::new();
//       41:a2|  let app = App::build(config);
//   >>  42:e1|  app.run().expect("failed to start");
//       43:b4|  println!("Done");
```

Custom `Sink` implementation that formats each match with hashline prefixes:
- `>>` prefix for matching lines
- `  ` prefix for context lines
- All lines include `LINE:HASH|content`

#### 4. Find Tool

**File**: `crates/rho-tools/src/find.rs`

Uses `ignore::WalkBuilder` + `globset::Glob`.

```rust
use ignore::WalkBuilder;
use globset::Glob;

pub struct FindTool {
    working_dir: PathBuf,
}

// Parameters: pattern (glob string), limit (optional, default 200)
// Behavior:
// 1. Parse pattern to extract base directory (e.g., "src/**/*.rs" → base="src/")
// 2. Walk with gitignore awareness
// 3. Match against glob
// 4. Sort by modification time (most recent first)
// 5. Return up to limit results
// 6. Timeout after 5 seconds
```

#### 5. Configurable Tool Backends

**File**: `crates/rho-tools/src/backend.rs`

Trait system for swappable tool implementations.

```rust
#[async_trait]
pub trait GrepBackend: Send + Sync {
    async fn search(&self, req: GrepRequest) -> Result<Vec<GrepMatch>>;
}

pub struct NativeGrep;  // Uses grep-searcher crates
pub struct ExternalGrep { binary: String, args: Vec<String> }  // Shells out

#[async_trait]
pub trait FindBackend: Send + Sync {
    async fn find(&self, req: FindRequest) -> Result<Vec<FindResult>>;
}

pub struct NativeFind;   // Uses ignore + globset
pub struct ExternalFind { binary: String }  // Shells out to fd/find
```

**File**: `crates/rho-tools/src/config.rs`

```rust
#[derive(Debug, Deserialize)]
pub struct ToolsConfig {
    pub grep: Option<GrepConfig>,
    pub find: Option<FindConfig>,
}

#[derive(Debug, Deserialize)]
pub struct GrepConfig {
    pub backend: BackendType,  // "native" or "external"
    pub binary: Option<String>,
}

pub fn load_tools_config() -> ToolsConfig {
    // 1. Try ~/.config/rho/tools.toml
    // 2. If not found, auto-detect and write defaults
}
```

### Success Criteria:

#### Automated Verification:
- [ ] `cargo test -p rho-tools` — unit tests for each tool:
  - Read: hashline-formatted output matches expected format
  - Edit: hashline edits apply correctly, replace mode works
  - Grep: matches found with correct hashline formatting
  - Find: glob matching with gitignore awareness
  - Backend: native and external implementations produce same results
- [ ] `cargo build` succeeds
- [ ] `cargo clippy` clean

#### Manual Verification:
- [ ] `cargo run -- "Read src/main.rs"` shows hashline format (`1:a3|fn main...`)
- [ ] `cargo run -- "Search for 'tokio' in the codebase"` returns grep results with hashline formatting
- [ ] `cargo run -- "Find all .rs files"` returns gitignore-aware results sorted by mtime
- [ ] `cargo run -- "Read src/main.rs, then change the first line"` uses hashline anchors in the edit
- [ ] Edit with stale anchors returns a mismatch error with `>>>` markers and correct refs

**Implementation Note**: Pause for manual verification. This is the point where the agent becomes fully functional with all 6 tools.

---

## Phase 6: OAuth + Claude Code Identity

### Overview
Implement the full PKCE OAuth flow in `anthropic-auth`, add Claude Code identity headers to the provider, and build the standalone `anthropic-auth` CLI binary.

### Changes Required:

#### 1. PKCE OAuth Flow

**File**: `crates/anthropic-auth/src/oauth.rs`

```rust
use sha2::{Sha256, Digest};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};

const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
const TOKEN_URL: &str = "https://console.anthropic.com/v1/oauth/token";
const REDIRECT_URI: &str = "https://console.anthropic.com/oauth/code/callback";
const SCOPES: &str = "org:create_api_key user:profile user:inference";

pub async fn login() -> Result<OAuthCredentials> {
    // 1. Generate PKCE verifier (43-128 random URL-safe chars)
    let verifier = generate_verifier();
    // 2. SHA-256 hash → base64url → challenge
    let challenge = generate_challenge(&verifier);
    // 3. Build authorize URL with query params
    let auth_url = format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&scope={}&code_challenge={}&code_challenge_method=S256",
        AUTHORIZE_URL, CLIENT_ID, REDIRECT_URI, SCOPES, challenge
    );
    // 4. Open browser
    open::that(&auth_url)?;
    // 5. Start local HTTP server to catch callback, or prompt for code
    let code = wait_for_auth_code().await?;
    // 6. Exchange code at TOKEN_URL
    let creds = exchange_code(&code, &verifier).await?;
    // 7. Save to config
    save_credentials(&creds)?;
    Ok(creds)
}
```

#### 2. Token Management

**File**: `crates/anthropic-auth/src/token.rs`

```rust
pub async fn get_token() -> Result<String> {
    // Priority:
    // 1. ANTHROPIC_API_KEY env var → return directly
    // 2. Load from ~/.config/anthropic-auth/auth.json
    //    a. If api_key type → return key
    //    b. If oauth type → check expiry, refresh if needed (with file lock)
    // 3. Error: no auth configured
}

async fn refresh_token(creds: &mut OAuthCredentials) -> Result<()> {
    // 1. Acquire file lock (fd-lock or fs2)
    // 2. Re-read file (another instance may have refreshed)
    // 3. If still expired, POST to TOKEN_URL with grant_type=refresh_token
    // 4. Save new access_token + refresh_token + expires
    // 5. Release lock
}
```

#### 3. Claude Code Identity Headers

**File**: `crates/rho-provider/src/lib.rs` (update)

When `is_oauth_token(token)` is true:
- `Authorization: Bearer {token}` (not `x-api-key`)
- `anthropic-beta: claude-code-20250219,oauth-2025-04-20`
- `user-agent: claude-cli/2.1.2 (external, cli)`
- `x-app: cli`
- Prepend system prompt: "You are Claude Code, Anthropic's official CLI for Claude."
- Normalize tool names: `Write` → `Write`, `Bash` → `Bash`, etc. (our tools already use these names)

When API key:
- `x-api-key: {key}` (standard header)
- No identity headers

#### 4. Standalone CLI Binary

**File**: `crates/anthropic-auth/src/bin/main.rs`

```rust
#[derive(Parser)]
enum Command {
    /// Log in with Claude Pro/Max subscription
    Login,
    /// Print current access token
    Token,
    /// Show auth status
    Status,
}

fn main() {
    match Command::parse() {
        Command::Login => { oauth::login().await?; println!("Logged in!"); }
        Command::Token => { println!("{}", token::get_token().await?); }
        Command::Status => { /* show token type, expiry, etc. */ }
    }
}
```

### Success Criteria:

#### Automated Verification:
- [ ] `cargo build --bin anthropic-auth` produces the standalone binary
- [ ] `cargo test -p anthropic-auth` — unit tests:
  - PKCE verifier/challenge generation matches known test vectors
  - Token file loading/saving roundtrips correctly
  - `is_oauth_token()` detects sk-ant-oat prefix
  - Refresh lock prevents concurrent refresh
- [ ] `cargo test -p rho-provider` — OAuth header injection tests

#### Manual Verification:
- [ ] `cargo run --bin anthropic-auth -- login` opens browser, completes OAuth flow
- [ ] `cargo run --bin anthropic-auth -- token` prints a `sk-ant-oat-...` token
- [ ] `cargo run --bin anthropic-auth -- status` shows token type and expiry
- [ ] `cargo run -- "hello"` works with OAuth token (no ANTHROPIC_API_KEY set)
- [ ] Token refresh works when token expires

**Implementation Note**: Pause for manual verification. OAuth flow requires a Claude Pro/Max subscription to test.

---

## Phase 7: CLI Polish + Config

### Overview
Configuration loading, system prompt, terminal output formatting, error handling, and graceful shutdown.

### Changes Required:

#### 1. Configuration

**File**: `src/config.rs`

```rust
#[derive(Debug, Deserialize)]
pub struct Config {
    pub model: Option<String>,         // Default: "claude-sonnet-4-5-20250929"
    pub thinking: Option<ThinkingLevel>,
    pub system_prompt: Option<String>,  // Prepended to default
    pub max_tokens: Option<usize>,
}

// Load from: CLI args > ~/.config/rho/config.toml > defaults
```

#### 2. System Prompt

Minimal, per the design principle. Models already know how to code from RL training.

```rust
const DEFAULT_SYSTEM_PROMPT: &str = r#"You are a coding assistant. You have access to tools for reading, editing, and searching files, and for running shell commands. Use them to help the user with their coding tasks."#;

// When OAuth: prepend "You are Claude Code, Anthropic's official CLI for Claude.\n\n"
```

#### 3. Terminal Output

**File**: `src/output.rs`

Streaming output with visual structure:
- Text deltas print to stdout immediately
- Tool calls show name + args in a styled block
- Tool results show a summary
- Thinking content hidden by default (show with `--show-thinking`)
- Ctrl+C handling: cancel current operation, allow sending a steering message

#### 4. Error Handling

- Network errors: retry with exponential backoff (max 3 retries)
- Auth errors: suggest `anthropic-auth login` or check `ANTHROPIC_API_KEY`
- Tool errors: format as tool result with `is_error: true`, let the model retry
- Rate limiting: respect `Retry-After` header

#### 5. CLI Arguments

```rust
#[derive(Parser)]
#[command(name = "rho", about = "A coding agent")]
struct Args {
    /// The prompt to send
    prompt: String,
    /// Model to use
    #[arg(short, long, default_value = "claude-sonnet-4-5-20250929")]
    model: String,
    /// Thinking level
    #[arg(long, default_value = "medium")]
    thinking: ThinkingLevel,
    /// Show thinking content
    #[arg(long)]
    show_thinking: bool,
    /// API key (overrides env/config)
    #[arg(long)]
    api_key: Option<String>,
    /// Working directory
    #[arg(short = 'C', long)]
    directory: Option<PathBuf>,
}
```

### Success Criteria:

#### Automated Verification:
- [ ] `cargo build --release` produces optimized binaries
- [ ] `cargo test` — all tests pass
- [ ] `cargo clippy` — clean
- [ ] Binary size check: `rho` binary < 20MB, `anthropic-auth` < 10MB

#### Manual Verification:
- [ ] `rho "explain this codebase"` streams a coherent response
- [ ] `rho -m claude-opus-4-6 "refactor main.rs"` uses specified model
- [ ] `rho --show-thinking "think about this"` shows thinking blocks
- [ ] Ctrl+C during execution stops cleanly
- [ ] `~/.config/rho/tools.toml` is auto-created on first run
- [ ] Error messages are helpful (auth errors, network errors, tool errors)

---

## Phase 8: Integration Testing

### Overview
End-to-end tests, edge case coverage, and performance validation.

### Changes Required:

#### 1. Integration Test Suite

**File**: `tests/integration/`

```rust
// Test: full agent loop with mock provider
#[tokio::test]
async fn test_write_tool_e2e() {
    // Mock provider returns a tool call for Write
    // Verify file is created
}

#[tokio::test]
async fn test_hashline_edit_e2e() {
    // Create a file, mock provider returns read + edit sequence
    // Verify edit applied correctly with hashline anchors
}

#[tokio::test]
async fn test_multi_turn_conversation() {
    // Mock provider returns multiple tool calls across turns
    // Verify all tools executed and results fed back
}

#[tokio::test]
async fn test_steering_interrupts() {
    // Inject a steering message during tool execution
    // Verify remaining tools are skipped
}
```

#### 2. Hashline Edge Cases

- Empty file editing
- Single-line file editing
- Unicode content (CJK, emoji, RTL)
- Files with mixed line endings (CRLF + LF)
- BOM-prefixed files
- Very long lines (>10K chars)
- All 7 heuristics with dedicated test cases
- Hash collision scenarios (same hash, different content)
- Concurrent edit attempts

#### 3. Provider Mock

**File**: `crates/rho-provider/src/mock.rs`

A mock provider that returns scripted responses for testing without hitting the real API.

```rust
pub struct MockProvider {
    responses: Vec<Message>,
}

impl MockProvider {
    pub fn new(responses: Vec<Message>) -> Self { ... }
}
```

### Success Criteria:

#### Automated Verification:
- [ ] `cargo test` — all unit and integration tests pass
- [ ] `cargo test -- --ignored` — slow/network tests pass
- [ ] `cargo clippy` — clean with all warnings addressed
- [ ] `cargo fmt -- --check` — formatting is consistent
- [ ] No `unsafe` outside of well-documented, minimal FFI boundaries

---

## Testing Strategy

### Unit Tests:
- **rho-hashline**: Hash computation, formatting, edit parsing, application algorithm, each heuristic, mismatch errors, relocation
- **rho-tools**: Each tool in isolation with filesystem fixtures
- **rho-provider**: SSE parsing with recorded event streams
- **anthropic-auth**: PKCE generation, token management, file locking
- **rho-core**: Event stream, type serialization, agent loop with mock provider

### Integration Tests:
- Full agent loop: prompt → provider → tool → result → loop
- Multi-turn conversations with tool sequences
- Steering message interruption
- Error recovery (tool failure, provider error, auth expiry)

### Manual Testing:
- Real API calls against Claude (Sonnet for speed, Opus for capability)
- OAuth login flow with a real Claude Pro/Max subscription
- Large file operations (>1MB files)
- Complex editing scenarios (multiple overlapping edits)
- Network failure scenarios (disconnect mid-stream)

## Performance Considerations

- **Startup time**: Target <50ms to first output. No lazy initialization on the critical path.
- **Streaming latency**: Text deltas should appear within 1ms of receipt from the API. Use unbuffered stdout.
- **Large file reading**: Use `memmap2` for files > 1MB. Stream hashlines in chunks.
- **Grep performance**: The ripgrep crate internals are already optimized. No additional work needed.
- **Binary size**: `cargo build --release` with `strip = true` and `lto = true` in profile.

```toml
[profile.release]
strip = true
lto = true
codegen-units = 1
```

## References

- Research document: `thoughts/shared/research/2026-02-13-pycodingagent-port-research.md`
- Reference implementation: `reference/upstream/` (core) + `reference/fork/` (tools)
- Crate selections: `reference/FAST-TOOLS-AND-HASHLINE.md`
- OAuth spec: `reference/upstream/claude-code-auth/CLAUDE-CODE-AUTH.md`
- Blog posts: see `reference/REFERENCES.md`
