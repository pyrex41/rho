# rho

A fast, extensible AI coding agent built in Rust. Rho gives an LLM a set of developer tools — file editing, shell access, web search, sub-agents — and runs an autonomous loop to complete coding tasks.

## Features

- **Multi-provider support** — Anthropic Claude, xAI Grok, and any OpenAI-compatible API (Groq, Ollama, etc.)
- **Rich tool set** — Read/write/edit files, run shell commands, regex search, glob find, web fetch & search, sub-agent tasks
- **LINE:HASH editing** — Hash-based line anchoring for precise, resilient file edits
- **Session persistence** — SQLite-backed sessions with full history and resume support
- **Autonomous loop** — Plan and build modes for hands-off multi-step execution
- **Autoresearch** — Autonomous optimization loop: iteratively improve a metric via code changes, benchmarking, and git
- **Extended thinking** — First-class support for reasoning models (Claude Opus, Grok reasoning)
- **Project config** — Per-project settings via `RHO.md` or `CLAUDE.md` with YAML frontmatter
- **Post-tool hooks** — Run validation commands (tests, lints) automatically after tool use
- **Skills & commands** — Auto-discover project-specific skills and slash commands
- **Streaming output** — Text or JSON streaming for integration with other tools

## Installation

### From source (CLI + GUI)

```bash
git clone https://github.com/pyrex41/rho.git
cd rho
cargo install --path .
cargo install --path crates/rho-gui
```

This installs both `rho-cli` and `rho` (GUI) binaries.

### Install script (CLI + GUI)

```bash
curl -fsSL https://raw.githubusercontent.com/pyrex41/rho/main/install.sh | bash
```

This downloads the latest pre-built binaries for both `rho-cli` and `rho` (GUI).

### Pre-built binaries

Download from [GitHub Releases](https://github.com/pyrex41/rho/releases). Pre-built binaries are provided for both the CLI (`rho-cli`) and GUI (`rho`).

The GUI provides a rich native interface with model picker, markdown rendering, autocomplete, and visual tool call display.

## Quick start

```bash
# Set your API key
export ANTHROPIC_API_KEY="sk-..."
```

**CLI:**

```bash
# One-shot prompt
rho-cli "add error handling to src/main.rs"

# Or use GUI
rho
```

The GUI provides an interactive chat with model selection, history, and visual tool call display.
## Usage

```
rho-cli [OPTIONS] [PROMPT]
rho-cli loop [OPTIONS]
rho-cli autoresearch --benchmark <CMD> --metric <NAME> [OPTIONS]
```

### Options

| Flag | Description | Default |
|------|-------------|---------|
| `--model <ID>` | Model registry ID or raw model ID | `claude-sonnet` |
| `--thinking <LEVEL>` | Thinking level: off, minimal, low, medium, high | `off` |
| `--show-thinking` | Display thinking output on stderr | |
| `--api-key <KEY>` | Override API key | env var / keychain |
| `-C, --directory <PATH>` | Working directory | current dir |
| `--prompt-file <FILE>` | Read prompt from file | |
| `--resume <SESSION>` | Resume an existing session | |
| `--tools <LIST>` | Restrict available tools (comma-separated) | all |
| `--system-append <TEXT>` | Append to the system prompt | |
| `--output-format <FMT>` | `text` or `stream-json` | `text` |

### Autonomous loop

```bash
# Build mode — execute tasks from an implementation plan
rho-cli loop

# Plan mode — create or refine IMPLEMENTATION_PLAN.md
rho-cli loop --mode plan

# Custom plan file, more iterations
rho-cli loop --plan my-plan.md --max-iterations 100
```

### Autoresearch

An autonomous optimization loop inspired by [pi-autoresearch](https://github.com/davebcn87/pi-autoresearch). The agent iteratively makes code changes to improve a measurable metric — the runner benchmarks, commits improvements, reverts regressions, and persists state for resumability.

```bash
# Optimize a benchmark metric (lower is better by default)
rho-cli autoresearch \
  --benchmark "cargo bench --bench parse" \
  --metric latency_ns \
  --metric-regex "latency_ns\s+(\d+)" \
  --direction lower

# Maximize a score, with correctness checks before benchmarking
rho-cli autoresearch \
  --benchmark "./bench.sh" \
  --metric score \
  --metric-regex "score:\s*(\d+\.?\d*)" \
  --direction higher \
  --checks "cargo test" \
  --max-iterations 20

# Use wall-clock time as the metric (omit --metric-regex)
rho-cli autoresearch \
  --benchmark "cargo build --release" \
  --metric build_time \
  --direction lower
```

| Flag | Description | Default |
|------|-------------|---------|
| `--benchmark <CMD>` | Command to measure the metric (required) | |
| `--metric <NAME>` | Name of the metric being optimized (required) | |
| `--direction <DIR>` | `lower` or `higher` | `lower` |
| `--metric-regex <RE>` | Regex with capture group to extract metric from output | wall-clock time |
| `--checks <CMD>` | Correctness check (tests/lint) run before benchmarking | |
| `--max-iterations <N>` | Maximum optimization iterations | `50` |
| `--sleep <SECS>` | Seconds between iterations | `5` |
| `--objective <TEXT>` | Description of the optimization goal | |
| `--benchmark-timeout <SECS>` | Kill benchmark after this many seconds | `300` |
| `--output-format <FMT>` | `text` or `stream-json` | `text` |

**How it works:**

1. Runs a baseline benchmark to capture the starting metric value
2. Each iteration: the agent analyzes experiment history, hypothesizes an optimization, and implements one focused code change
3. The runner validates changes (optional `--checks`), then benchmarks
4. If the metric improved: changes are committed. If it regressed: changes are reverted
5. State is persisted in `autoresearch.jsonl` (structured log) and `autoresearch.md` (session document with strategy notes)
6. Resumable — kill and restart anytime; it picks up from the JSONL log
7. The agent can write `EXHAUSTED` to `.stop` when no more optimizations are viable

**Output files:**

- `autoresearch.jsonl` — One JSON object per experiment (iteration, metric value, status, commit hash)
- `autoresearch.md` — Human-readable session document with config, best result, history table, and agent strategy notes

## Built-in tools

| Tool | Description |
|------|-------------|
| `read` | Read files with `LINE:HASH` format, or list directories |
| `write` | Create or overwrite files |
| `edit` | Edit files using `LINE:HASH` anchors or text replacement |
| `bash` | Execute shell commands (PTY, up to 1 hour timeout) |
| `grep` | Search file contents with regex (respects `.gitignore`) |
| `find` | Find files by glob pattern (respects `.gitignore`) |
| `task` | Launch a sub-agent in a separate context |
| `web_fetch` | Fetch a URL and convert to markdown |
| `web_search` | Search the web via DuckDuckGo (no API key needed) |

## Supported models

### Anthropic Claude
- `claude-sonnet` — Claude Sonnet 4.6 (default)
- `claude-opus` — Claude Opus 4.6 (with extended thinking)
- `claude-haiku` — Claude Haiku 4.5

### OpenAI
- `gpt-5.4` — Latest flagship model with 1M context
- `gpt-5.4-mini` — Strong mini model for coding and agents
- `gpt-5.4-nano` — Efficient for high-volume tasks

### xAI Grok
- `grok-3`, `grok-3-mini`, `grok-2`
- `grok-4.20-reasoning`, `grok-4.20-non-reasoning` (experimental beta)
- `grok-4.20-multi-agent` (multi-agent responses endpoint)
- Additional experimental: `grok-code-fast-1`, `grok-4-1-reasoning`, etc.

The built-in models are kept up-to-date in `crates/rho-core/src/models.rs`. Zen models (via `OPENCODE_ZEN_API_KEY`) provide additional latest options prefixed with `zen-`.

### Local Models (Ollama, MLX, etc.)
Rho has generic support for any OpenAI-compatible local server via custom config.

**Ollama example:**
```bash
# Start Ollama and pull model
ollama serve
ollama pull gemma2:9b
```

Then add to `~/.rho/models.toml`:
```toml
[[model]]
id = "ollama-gemma"
provider = "openai"
model_id = "gemma2:9b"
base_url = "http://localhost:11434/v1"
context_window = 8192
max_tokens = 4096
# No api_key needed for localhost
```

**MLX (for ../gemma or other local models on Apple Silicon):**
```bash
# Install and run MLX server pointing to your model dir
pip install mlx-lm
mlx_lm.server --model ../gemma --port 8080
```

Then:
```toml
[[model]]
id = "mlx-gemma"
provider = "openai"
model_id = "gemma"  # adjust to what the server expects
base_url = "http://localhost:8080/v1"
context_window = 8192
max_tokens = 4096
```

Localhost base URLs automatically use `"local"` as the API key (no auth required).

## Configuration

Create a `RHO.md` (or `CLAUDE.md`) in your project root:

```markdown
---
model: claude-sonnet
thinking: high
memories: true
compact_threshold: 0.7
validation_commands: ["cargo test", "cargo clippy"]
post_tools_hooks:
  - name: tests
    command: cargo test
    timeout: 60
    inject_on_failure: true
    trigger_tools: [write, edit]
---

Additional system prompt instructions go here as markdown.
```

### Environment variables

| Variable | Description |
|----------|-------------|
| `ANTHROPIC_API_KEY` | Anthropic API key |
| `XAI_API_KEY` | xAI / Grok API key |
| `OPENAI_API_KEY` | OpenAI API key |
| `RHO_SESSION_DB` | Custom path for the session database |
| `RUST_LOG` | Tracing filter (default: `warn`) |

## Project structure

```
crates/
├── rho-core        # Agent loop, types, config, event system
├── rho-provider    # LLM provider implementations (Anthropic, OpenAI-compat)
├── rho-tools       # Built-in tool implementations
├── rho-hashline    # LINE:HASH file anchoring
├── rho-server      # HTTP server mode
└── rho-gui         # GUI frontend (iced, in development)
src/
├── main.rs         # CLI entry point
├── loop_runner.rs  # Autonomous loop logic
└── autoresearch.rs # Autoresearch optimization loop
```

## License

MIT
