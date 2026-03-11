# rho

A fast, extensible AI coding agent built in Rust. Rho gives an LLM a set of developer tools — file editing, shell access, web search, sub-agents — and runs an autonomous loop to complete coding tasks.

## Features

- **Multi-provider support** — Anthropic Claude, xAI Grok, and any OpenAI-compatible API (Groq, Ollama, etc.)
- **Rich tool set** — Read/write/edit files, run shell commands, regex search, glob find, web fetch & search, sub-agent tasks
- **LINE:HASH editing** — Hash-based line anchoring for precise, resilient file edits
- **Session persistence** — SQLite-backed sessions with full history and resume support
- **Autonomous loop** — Plan and build modes for hands-off multi-step execution
- **Extended thinking** — First-class support for reasoning models (Claude Opus, Grok reasoning)
- **Project config** — Per-project settings via `RHO.md` or `CLAUDE.md` with YAML frontmatter
- **Post-tool hooks** — Run validation commands (tests, lints) automatically after tool use
- **Skills & commands** — Auto-discover project-specific skills and slash commands
- **Streaming output** — Text or JSON streaming for integration with other tools

## Installation

### From source

```bash
git clone https://github.com/pyrex41/rho.git
cd rho
cargo install --path .
```

### Install script

```bash
curl -fsSL https://raw.githubusercontent.com/pyrex41/rho/main/install.sh | bash
```

This detects your OS and architecture, downloads the latest release binary, and installs it to `~/.local/bin` (or a directory you specify with `RHO_INSTALL_DIR`).

### Pre-built binaries

Download from [GitHub Releases](https://github.com/pyrex41/rho/releases). Binaries are available for:
- Linux x86_64 / ARM64
- macOS x86_64 / ARM64 (Apple Silicon)
- Windows x86_64

## Quick start

```bash
# Set your API key
export ANTHROPIC_API_KEY="sk-..."

# Run a one-shot prompt
rho-cli "add error handling to src/main.rs"

# Use a specific model
rho-cli --model claude-opus "refactor the auth module"

# Enable extended thinking
rho-cli --thinking high "design a caching layer"

# Resume a previous session interactively
rho-cli --resume <session-id>
```

## Usage

```
rho-cli [OPTIONS] [PROMPT]
rho-cli loop [OPTIONS]
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
- `claude-sonnet` — Claude Sonnet 4.5 (default)
- `claude-opus` — Claude Opus 4.6 (with extended thinking)
- `claude-haiku` — Claude Haiku 4.5

### xAI Grok
- `grok-3`, `grok-3-mini`, `grok-2`
- `grok-4.20-reasoning`, `grok-4.20-non-reasoning` (experimental)

### Custom models

Add models via `~/.rho/models.toml`:

```toml
[[model]]
id = "gpt-4o"
provider = "openai"
model_id = "gpt-4o"
api_key_env = "OPENAI_API_KEY"
context_window = 128000
max_tokens = 16384
thinking = false
```

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
└── loop_runner.rs  # Autonomous loop logic
```

## License

MIT
