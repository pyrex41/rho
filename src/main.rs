use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tokio_util::sync::CancellationToken;

use rho_core::agent_loop::{agent_loop, AgentLoopConfig};
use rho_core::compaction;
use rho_core::config::load_project_config;
use rho_core::event_handler::{handle_event, EventHandlerConfig, SessionPersistence};
use rho_core::models::{ModelConfig, ModelRegistry, ProviderType};
use rho_core::tool::AgentTool;
use rho_core::types::*;
use rho_core::session::SessionStore;

mod autoresearch;
mod llama_ux;
mod loop_runner;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OutputFormat {
    Text,
    StreamJson,
}

impl OutputFormat {
    fn parse(s: &str) -> Self {
        match s {
            "stream-json" | "json" => OutputFormat::StreamJson,
            _ => OutputFormat::Text,
        }
    }
}

#[derive(Parser)]
#[command(name = "rho", about = "AI coding agent with file tools", args_conflicts_with_subcommands = true)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// The prompt to send to the agent
    prompt: Option<String>,

    /// Model ID to use (registry ID like "claude-sonnet", or raw model ID)
    #[arg(long, default_value = "claude-sonnet")]
    model: String,

    /// Thinking level (off, minimal, low, medium, high)
    #[arg(long, default_value = "off")]
    thinking: String,

    /// Show thinking output on stderr
    #[arg(long)]
    show_thinking: bool,

    /// Override API key (default: env var or keychain)
    #[arg(long)]
    api_key: Option<String>,

    /// Working directory
    #[arg(short = 'C', long)]
    directory: Option<PathBuf>,

    /// Read prompt from file
    #[arg(long)]
    prompt_file: Option<PathBuf>,

    /// Resume an existing session ID (prints/updates events in that session)
    #[arg(long, alias = "session-id")]
    resume: Option<String>,

    /// Restrict available tools (comma-separated names)
    #[arg(long, value_delimiter = ',')]
    tools: Option<Vec<String>>,

    /// Append to system prompt
    #[arg(long)]
    system_append: Option<String>,

    /// Output format: text or stream-json
    #[arg(long, default_value = "text")]
    output_format: String,

    /// Load pre-seeded messages from a JSON context file (for cache-optimized subagent forking)
    #[arg(long)]
    context_file: Option<PathBuf>,
}

#[derive(Subcommand)]
enum AuthCommands {
    /// Log in via OAuth (opens browser)
    Login,
    /// Print the current API token
    Token,
    /// Show authentication status
    Status,
}

#[derive(Subcommand)]
enum Commands {
    /// Manage Anthropic authentication (login, token, status)
    Auth {
        #[command(subcommand)]
        command: AuthCommands,
    },
    /// Start HTTP server mode
    Server {
        /// Host to bind to
        #[arg(long, default_value = "0.0.0.0")]
        host: String,

        /// Port to listen on
        #[arg(long, default_value_t = 3000)]
        port: u16,

        /// Optional bearer token for authentication
        #[arg(long)]
        bearer_token: Option<String>,

        /// Model ID to use
        #[arg(long)]
        model: Option<String>,

        /// Thinking level
        #[arg(long)]
        thinking: Option<String>,

        /// Override API key
        #[arg(long)]
        api_key: Option<String>,

        /// Working directory
        #[arg(short = 'C', long)]
        directory: Option<PathBuf>,
    },
    /// Run autonomous loop (Ralph pattern)
    Loop {
        /// Loop mode: "build" or "plan"
        #[arg(long, default_value = "build")]
        mode: String,

        /// Path to the implementation plan
        #[arg(long, default_value = "IMPLEMENTATION_PLAN.md")]
        plan: PathBuf,

        /// Maximum number of iterations
        #[arg(long, default_value_t = 50)]
        max_iterations: usize,

        /// Seconds to sleep between iterations
        #[arg(long, default_value_t = 5)]
        sleep: u64,

        /// Model ID override
        #[arg(long)]
        model: Option<String>,

        /// Thinking level override
        #[arg(long)]
        thinking: Option<String>,

        /// Override API key
        #[arg(long)]
        api_key: Option<String>,

        /// Working directory
        #[arg(short = 'C', long)]
        directory: Option<PathBuf>,

        /// Output format: text or stream-json
        #[arg(long, default_value = "text")]
        output_format: String,
    },
    /// Run autonomous optimization loop
    Autoresearch {
        /// Benchmark command to measure the metric
        #[arg(long)]
        benchmark: String,

        /// Name of the metric being optimized
        #[arg(long)]
        metric: String,

        /// Optimization direction: "lower" or "higher"
        #[arg(long, default_value = "lower")]
        direction: String,

        /// Regex to extract metric from benchmark stdout (first capture group).
        /// If omitted, wall-clock time of benchmark is used as the metric.
        #[arg(long)]
        metric_regex: Option<String>,

        /// Correctness check command (tests/lint), run before benchmarking
        #[arg(long)]
        checks: Option<String>,

        /// Maximum iterations
        #[arg(long, default_value_t = 50)]
        max_iterations: usize,

        /// Seconds between iterations
        #[arg(long, default_value_t = 5)]
        sleep: u64,

        /// Optimization objective description
        #[arg(long)]
        objective: Option<String>,

        /// Benchmark timeout in seconds
        #[arg(long, default_value_t = 300)]
        benchmark_timeout: u64,

        /// Model ID override
        #[arg(long)]
        model: Option<String>,

        /// Thinking level override
        #[arg(long)]
        thinking: Option<String>,

        /// Override API key
        #[arg(long)]
        api_key: Option<String>,

        /// Working directory
        #[arg(short = 'C', long)]
        directory: Option<PathBuf>,

        /// Output format: text or stream-json
        #[arg(long, default_value = "text")]
        output_format: String,
    },
    /// Manage local models (setup, list)
    Models {
        #[command(subcommand)]
        command: ModelsCommands,
    },
}

#[derive(Subcommand, Debug)]
enum ModelsCommands {
    /// Set up a local model via Ollama
    Setup {
        /// Model to set up (e.g. "gemma-4-e4b"). Omit to see available models.
        model: Option<String>,
    },
    /// List all registered models and their availability
    List,
    /// List running llama.cpp servers (managed by rho)
    Running,
    /// Stop a running llama.cpp server
    Stop {
        /// Model id to stop (as defined in ~/.rho/models.toml)
        id: Option<String>,
        /// Stop every running llama.cpp server
        #[arg(long)]
        all: bool,
    },
    /// Pre-spawn a llama.cpp server (warmup) without running an agent
    Warmup {
        /// Model id to warm up
        id: String,
    },
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

fn format_uptime(d: Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m{}s", secs / 60, secs % 60)
    } else if secs < 86_400 {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    } else {
        format!("{}d{}h", secs / 86_400, (secs % 86_400) / 3600)
    }
}

fn parse_thinking(s: &str) -> ThinkingLevel {
    match s.to_lowercase().as_str() {
        "minimal" => ThinkingLevel::Minimal,
        "low" => ThinkingLevel::Low,
        "medium" => ThinkingLevel::Medium,
        "high" => ThinkingLevel::High,
        _ => ThinkingLevel::Off,
    }
}

const SYSTEM_PROMPT: &str = "\
You are a coding assistant with tools for reading, editing, searching files and running commands.

Available tools:
- read: Read a file (returns LINE:HASH|content format) or list a directory
- write: Create or overwrite a file
- edit: Edit a file using LINE:HASH anchors from read output, or text replacement
- bash: Execute shell commands
- grep: Search file contents with regex (returns matches with LINE:HASH|content format)
- find: Find files by glob pattern (respects .gitignore)
- task: Launch a subagent to handle a task in a separate context
- web_fetch: Fetch a URL and return its content as clean markdown/text
- web_search: Search the web via DuckDuckGo and return results (title, URL, snippet)

When editing files, first read them to get LINE:HASH references, then use edit with those anchors. \
For new files, use write. For small changes, use edit. For running tests or builds, use bash.

Web search guidance:
- IMPORTANT: You MUST include the current year in search queries for recent information. \
For example, if asked about 'latest trending repos', search for 'trending github repos' with the year, NOT without a year.
- Use multiple searches with different queries to get comprehensive results. A single search is rarely enough.
- Fetch primary sources directly (e.g. github.com/trending, trendshift.io, official docs) rather than relying only on blog posts.
- After searching, use web_fetch on the most promising URLs to get detailed information.
- Always cite your sources with URLs in your response.";

fn build_tools(
    cwd: &PathBuf,
    allowed: &Option<Vec<String>>,
    agent_config: &rho_core::config::ProjectConfig,
) -> Vec<Arc<dyn AgentTool>> {
    let all_tools: Vec<Arc<dyn AgentTool>> = vec![
        Arc::new(rho_tools::read::ReadTool::with_cwd(cwd.clone())),
        Arc::new(rho_tools::write::WriteTool::with_cwd(cwd.clone())),
        Arc::new(rho_tools::edit::EditTool::with_cwd(cwd.clone())),
        Arc::new(rho_tools::bash::BashTool::new(cwd.clone())),
        Arc::new(rho_tools::grep::GrepTool::new(cwd.clone())),
        Arc::new(rho_tools::find::FindTool::new(cwd.clone())),
        Arc::new(rho_tools::task::TaskTool::new(
            cwd.clone(),
            agent_config.max_agent_depth,
            agent_config.agents.clone(),
        )),
        Arc::new(rho_tools::web_fetch::WebFetchTool::new()),
        Arc::new(rho_tools::web_search::WebSearchTool::new()),
    ];

    if let Some(ref allowed) = allowed {
        all_tools
            .into_iter()
            .filter(|t| allowed.iter().any(|a| a == t.name()))
            .collect()
    } else {
        all_tools
    }
}

/// Resolve a model name (registry ID or raw model ID) to a ModelConfig.
/// For raw IDs not in the registry, a synthetic config is created.
fn resolve_model_config(
    model_arg: &str,
    registry: &ModelRegistry,
    thinking: ThinkingLevel,
) -> ModelConfig {
    // OpenCode Zen: opencode/<model-id> → route to Zen gateway
    if let Some(model_id) = model_arg.strip_prefix("opencode/") {
        let (provider, base_url) = if model_id.contains("claude") {
            (ProviderType::Anthropic, "https://opencode.ai/zen".into())
        } else {
            (ProviderType::OpenAi, "https://opencode.ai/zen/v1".into())
        };
        return ModelConfig {
            id: model_arg.to_string(),
            provider,
            model_id: model_id.to_string(),
            base_url,
            api_key_env: Some("OPENCODE_ZEN_API_KEY".into()),
            context_window: 200_000,
            max_tokens: if thinking != ThinkingLevel::Off {
                16_384
            } else {
                8_192
            },
            thinking: model_id.contains("opus") || thinking != ThinkingLevel::Off,
            server_tools: None,
            llama_cpp: None,
        };
    }

    // Try exact match first
    if let Some(config) = registry.get(model_arg) {
        return config.clone();
    }

    // Strip provider prefix (e.g. "xai/grok-code-fast-1" → "grok-code-fast-1")
    let stripped = model_arg.split('/').last().unwrap_or(model_arg);
    if stripped != model_arg {
        if let Some(config) = registry.get(stripped) {
            return config.clone();
        }
    }

    // Fallback: treat as raw model ID, infer provider from name
    let (provider, api_key_env, base_url) = if model_arg.contains("claude") {
        (ProviderType::Anthropic, "ANTHROPIC_API_KEY".into(), String::new())
    } else if model_arg.contains("grok") {
        (ProviderType::OpenAi, "XAI_API_KEY".into(), "https://api.x.ai/v1".into())
    } else {
        (ProviderType::OpenAi, "OPENAI_API_KEY".into(), String::new())
    };

    ModelConfig {
        id: model_arg.to_string(),
        provider: provider.clone(),
        model_id: stripped.to_string(),
        base_url,
        api_key_env: Some(api_key_env),
        context_window: 200_000,
        max_tokens: if thinking != ThinkingLevel::Off {
            16_384
        } else {
            8_192
        },
        thinking: model_arg.contains("opus") || thinking != ThinkingLevel::Off,
        server_tools: None,
        llama_cpp: None,
    }
}

fn build_system_prompt(
    cwd: &PathBuf,
    config: &rho_core::config::ProjectConfig,
    system_append: Option<&str>,
) -> String {
    let base = config.system_prompt.as_deref().unwrap_or(SYSTEM_PROMPT);

    let mut prompt = base.to_string();

    // Inject current date/time and year
    let now = chrono::Local::now();
    let date_str = now.format("%Y-%m-%d %H:%M %Z").to_string();
    let month_year_str = now.format("%B %Y").to_string();
    let year_str = now.format("%Y").to_string();
    if !prompt.contains("current date") {
        prompt = format!(
            "{}\n\nThe current date is {}. The current month is {}. \
             You MUST use this year when searching for recent information.",
            prompt, date_str, month_year_str
        );
    }
    // Replace year placeholder in web search guidance
    prompt = prompt
        .replace(
            "the current year",
            &format!("the current year ({})", year_str),
        )
        .replace("with the year", &format!("with the year {}", year_str));

    // Add skills
    let skill_dirs = rho_core::skills::default_skill_dirs(cwd);
    let skills = rho_core::skills::discover_skills(&skill_dirs);
    if !skills.is_empty() {
        prompt.push_str("\n\n");
        prompt.push_str(&rho_core::skills::format_skills_prompt(&skills));
    }

    // Add memories
    if config.memories {
        let memory_dirs = rho_core::memories::default_memory_dirs(cwd);
        let memories = rho_core::memories::discover_memories(&memory_dirs);
        if !memories.is_empty() {
            prompt.push_str("\n\n");
            prompt.push_str(&rho_core::memories::format_memories_prompt(&memories));
        }
    }

    // Add commands
    let commands = rho_core::commands::all_commands(cwd);
    if !commands.is_empty() {
        prompt.push_str("\n\n<available_commands>\n");
        for cmd in &commands {
            prompt.push_str(&format!("  /{} — {}\n", cmd.name, cmd.description));
        }
        prompt.push_str("</available_commands>");
    }

    // Project config append
    if let Some(append) = &config.system_prompt_append {
        prompt.push_str("\n\n");
        prompt.push_str(append);
    }

    // CLI --system-append
    if let Some(append) = system_append {
        prompt.push_str("\n\n");
        prompt.push_str(append);
    }

    prompt
}

fn session_db_path(cwd: &PathBuf) -> PathBuf {
    if let Ok(path) = std::env::var("RHO_SESSION_DB") {
        return PathBuf::from(path);
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".rho").join("sessions.db");
    }
    cwd.join(".rho").join("sessions.db")
}

fn read_prompt_line() -> Result<Option<String>> {
    print!("rho> ");
    io::stdout().flush().ok();
    let mut input = String::new();
    let bytes = io::stdin()
        .read_line(&mut input)
        .context("Failed reading interactive prompt")?;
    if bytes == 0 {
        return Ok(None);
    }
    Ok(Some(input.trim().to_string()))
}

#[allow(clippy::too_many_arguments)]
async fn run_agent_turn(
    messages: Vec<Message>,
    model: Model,
    api_key: String,
    system_prompt: String,
    tools: Vec<Arc<dyn AgentTool>>,
    thinking: ThinkingLevel,
    model_config: &ModelConfig,
    compact_threshold: Option<f64>,
    show_thinking: bool,
    cancel: CancellationToken,
    session_handler: &mut Option<EventHandlerConfig>,
    last_reported_session_id: &mut Option<String>,
    output_format: OutputFormat,
    post_tools_hooks: Vec<Arc<dyn rho_core::hooks::PostToolsHook>>,
) -> Vec<Message> {
    let transform_messages = compact_threshold.map(|t| compaction::make_compaction_transform(t, None));
    let config = AgentLoopConfig {
        model,
        api_key,
        system_prompt,
        tools,
        thinking,
        max_tokens: None,
        stream_fn: rho_provider::stream_fn_for_model(model_config),
        get_steering_messages: None,
        get_follow_up_messages: None,
        transform_messages,
        post_tools_hooks,
        pre_tool_hooks: vec![],
        lifecycle_hooks: vec![],
        shared_messages: None,
    };

    let mut consumer = agent_loop(messages.clone(), config, cancel);
    let mut stdout = std::io::stdout();
    let mut final_messages = messages;

    while let Some(event) = consumer.next().await {
        if let Some(handler) = session_handler.as_mut() {
            if let Some(update) = handle_event(&event, handler) {
                if let Some(session_id) = update.session_id {
                    if last_reported_session_id.as_deref() != Some(session_id.as_str()) {
                        match output_format {
                            OutputFormat::Text => {
                                eprintln!("[session:{}]", session_id);
                            }
                            OutputFormat::StreamJson => {
                                println!("{}", serde_json::json!({
                                    "type": "session",
                                    "session_id": session_id
                                }));
                            }
                        }
                        *last_reported_session_id = Some(session_id);
                    }
                }
            }
        }

        match output_format {
            OutputFormat::Text => match event {
                AgentEvent::MessageUpdate { event, .. } => match event {
                    AssistantStreamEvent::TextDelta { delta, .. } => {
                        print!("{}", delta);
                        stdout.flush().ok();
                    }
                    AssistantStreamEvent::ThinkingDelta { delta, .. } => {
                        if show_thinking {
                            eprint!("{}", delta);
                            std::io::stderr().flush().ok();
                        }
                    }
                    _ => {}
                },
                AgentEvent::ToolExecutionStart {
                    tool_name, args, ..
                } => {
                    let args_summary = match serde_json::to_string(&args) {
                        Ok(s) if s.len() > 200 => format!("{}...", &s[..200]),
                        Ok(s) => s,
                        Err(_) => String::new(),
                    };
                    eprintln!("\n[tool:{}] {}", tool_name, args_summary);
                }
                AgentEvent::ToolExecutionEnd {
                    tool_name,
                    is_error,
                    result,
                    ..
                } => {
                    if is_error {
                        eprintln!("[tool:{}] ERROR: {:?}", tool_name, result.content);
                    } else {
                        eprintln!("[tool:{}] done", tool_name);
                    }
                }
                AgentEvent::ContextCompacted {
                    original_estimate,
                    compacted_estimate,
                    ..
                } => {
                    eprintln!(
                        "[compact] {} -> {} tokens (estimated)",
                        original_estimate, compacted_estimate
                    );
                }
                AgentEvent::PostToolsHookStart { hook_name } => {
                    eprintln!("[hook:{}] running...", hook_name);
                }
                AgentEvent::PostToolsHookEnd {
                    hook_name,
                    success,
                    summary,
                } => {
                    if success {
                        eprintln!("[hook:{}] {}", hook_name, summary);
                    } else {
                        eprintln!("[hook:{}] FAILED: {}", hook_name, summary);
                    }
                }
                AgentEvent::AgentEnd { messages } => {
                    final_messages = messages;
                    println!();
                }
                _ => {}
            },
            OutputFormat::StreamJson => match event {
                AgentEvent::MessageUpdate { event, .. } => match event {
                    AssistantStreamEvent::TextDelta { delta, .. } => {
                        println!("{}", serde_json::json!({
                            "type": "text_delta",
                            "text": delta
                        }));
                    }
                    _ => {}
                },
                AgentEvent::ToolExecutionStart {
                    tool_call_id,
                    tool_name,
                    args,
                } => {
                    let input_summary = match serde_json::to_string(&args) {
                        Ok(s) if s.len() > 200 => format!("{}...", &s[..200]),
                        Ok(s) => s,
                        Err(_) => String::new(),
                    };
                    println!("{}", serde_json::json!({
                        "type": "tool_start",
                        "tool_name": tool_name,
                        "tool_id": tool_call_id,
                        "input_summary": input_summary
                    }));
                }
                AgentEvent::ToolExecutionEnd {
                    tool_call_id,
                    tool_name,
                    is_error,
                    ..
                } => {
                    println!("{}", serde_json::json!({
                        "type": "tool_result",
                        "tool_name": tool_name,
                        "tool_id": tool_call_id,
                        "success": !is_error
                    }));
                }
                AgentEvent::PostToolsHookStart { hook_name } => {
                    println!("{}", serde_json::json!({
                        "type": "hook_start",
                        "hook_name": hook_name
                    }));
                }
                AgentEvent::PostToolsHookEnd {
                    hook_name,
                    success,
                    summary,
                } => {
                    println!("{}", serde_json::json!({
                        "type": "hook_result",
                        "hook_name": hook_name,
                        "success": success,
                        "summary": summary
                    }));
                }
                AgentEvent::AgentEnd { messages } => {
                    let sid = last_reported_session_id.clone().unwrap_or_default();
                    println!("{}", serde_json::json!({
                        "type": "complete",
                        "success": true,
                        "session_id": sid
                    }));
                    final_messages = messages;
                }
                _ => {}
            },
        }
    }

    final_messages
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Auth { command }) => {
            match command {
                AuthCommands::Login => {
                    match rho_core::auth::oauth::login().await {
                        Ok(_) => eprintln!("Login successful."),
                        Err(e) => {
                            eprintln!("Login failed: {e}");
                            std::process::exit(1);
                        }
                    }
                }
                AuthCommands::Token => {
                    match rho_core::auth::get_token() {
                        Ok(token) => print!("{token}"),
                        Err(e) => {
                            eprintln!("Error: {e}");
                            std::process::exit(1);
                        }
                    }
                }
                AuthCommands::Status => {
                    // Check env var
                    if let Ok(val) = std::env::var("ANTHROPIC_API_KEY") {
                        if !val.is_empty() {
                            if rho_core::auth::is_oauth_token(&val) {
                                println!("Authenticated via ANTHROPIC_API_KEY (OAuth token)");
                            } else {
                                println!("Authenticated via ANTHROPIC_API_KEY");
                            }
                            return Ok(());
                        }
                    }
                    // Check keychain / file credentials via get_token
                    match rho_core::auth::get_token() {
                        Ok(token) => {
                            if rho_core::auth::is_oauth_token(&token) {
                                println!("Authenticated via keychain/OAuth");
                            } else {
                                println!("Authenticated via keychain");
                            }
                        }
                        Err(_) => {
                            // Check file credentials specifically
                            if let Ok(creds) = rho_core::auth::oauth::load_credentials() {
                                if !creds.access_token.is_empty() {
                                    let expired = creds
                                        .expires_at
                                        .map(|exp| {
                                            let now = std::time::SystemTime::now()
                                                .duration_since(std::time::UNIX_EPOCH)
                                                .unwrap_or_default()
                                                .as_secs();
                                            now >= exp
                                        })
                                        .unwrap_or(false);
                                    if expired {
                                        println!("Authenticated via OAuth file credentials (token expired)");
                                    } else {
                                        println!("Authenticated via OAuth file credentials");
                                    }
                                    return Ok(());
                                }
                            }
                            println!("Not authenticated");
                        }
                    }
                }
            }
        }
        Some(Commands::Server {
            host,
            port,
            bearer_token,
            model,
            thinking,
            api_key,
            directory,
        }) => {
            let cwd = match directory {
                Some(dir) => std::fs::canonicalize(&dir)
                    .with_context(|| format!("Invalid directory: {}", dir.display()))?,
                None => std::env::current_dir().context("Failed to get current directory")?,
            };

            let project_config = load_project_config(&cwd);

            let model_id = model
                .or(project_config.model.clone())
                .unwrap_or_else(|| "claude-sonnet".into());
            let thinking_str = thinking.unwrap_or_else(|| {
                project_config
                    .thinking
                    .map(|t| format!("{:?}", t).to_lowercase())
                    .unwrap_or_else(|| "off".into())
            });
            let thinking_level = parse_thinking(&thinking_str);

            let registry = ModelRegistry::load();
            let model_config = resolve_model_config(&model_id, &registry, thinking_level);
            let model_config = llama_ux::prepare_with_ux(model_config).await?;

            let resolved_api_key = match api_key {
                Some(key) => key,
                None => {
                    ModelRegistry::resolve_api_key(&model_config).map_err(|e| anyhow::anyhow!(e))?
                }
            };

            let allowed_tools = project_config.allowed_tools.clone();
            let cwd_for_tools = cwd.clone();
            let project_config_for_tools = project_config.clone();

            let server_config = rho_server::ServerConfig {
                model_config,
                api_key: resolved_api_key,
                system_prompt: build_system_prompt(&cwd, &project_config, None),
                tools_factory: Arc::new(move || build_tools(&cwd_for_tools, &allowed_tools, &project_config_for_tools)),
                thinking: thinking_level,
                bearer_token,
                compact_threshold: project_config.compact_threshold,
                cwd: cwd.clone(),
            };

            rho_server::start_server_with_addr(server_config, &host, port).await?;
        }
        Some(Commands::Loop {
            mode,
            plan,
            max_iterations,
            sleep,
            model,
            thinking,
            api_key,
            directory,
            output_format,
        }) => {
            let cwd = match directory {
                Some(dir) => std::fs::canonicalize(&dir)
                    .with_context(|| format!("Invalid directory: {}", dir.display()))?,
                None => std::env::current_dir().context("Failed to get current directory")?,
            };

            let project_config = load_project_config(&cwd);

            let model_id = model
                .or(project_config.model.clone())
                .unwrap_or_else(|| "claude-sonnet".into());
            let thinking_str = thinking.unwrap_or_else(|| {
                project_config
                    .thinking
                    .map(|t| format!("{:?}", t).to_lowercase())
                    .unwrap_or_else(|| "off".into())
            });
            let thinking_level = parse_thinking(&thinking_str);

            let registry = ModelRegistry::load();
            let model_config = resolve_model_config(&model_id, &registry, thinking_level);
            let model_config = llama_ux::prepare_with_ux(model_config).await?;

            let api_key = match api_key {
                Some(key) => key,
                None => {
                    ModelRegistry::resolve_api_key(&model_config).map_err(|e| anyhow::anyhow!(e))?
                }
            };

            let model = ModelRegistry::to_model(&model_config);

            // Plan mode defaults to read-only tools
            let default_tools =
                if loop_runner::LoopMode::from_str(&mode) == loop_runner::LoopMode::Plan {
                    Some(vec![
                        "read".into(),
                        "grep".into(),
                        "find".into(),
                        "write".into(),
                    ])
                } else {
                    project_config.allowed_tools.clone()
                };
            let tools = build_tools(&cwd, &default_tools, &project_config);
            let system_prompt = build_system_prompt(&cwd, &project_config, None);

            let cancel = CancellationToken::new();
            let cancel_clone = cancel.clone();
            tokio::spawn(async move {
                tokio::signal::ctrl_c().await.ok();
                eprintln!("\nInterrupted, cancelling...");
                cancel_clone.cancel();
            });

            let post_tools_hooks: Vec<Arc<dyn rho_core::hooks::PostToolsHook>> = project_config
                .post_tools_hooks
                .iter()
                .map(|h| -> Arc<dyn rho_core::hooks::PostToolsHook> {
                    Arc::new(rho_core::hooks::ShellCommandHook {
                        config: h.clone(),
                        cwd: cwd.clone(),
                    })
                })
                .collect();

            let output_format = OutputFormat::parse(&output_format);

            let loop_config = loop_runner::LoopConfig {
                mode: loop_runner::LoopMode::from_str(&mode),
                plan_path: plan,
                max_iterations,
                sleep_between: Duration::from_secs(sleep),
                model,
                api_key,
                system_prompt,
                tools,
                thinking: thinking_level,
                validation_commands: project_config.validation_commands,
                cwd,
                stream_fn: rho_provider::stream_fn_for_model(&model_config),
                post_tools_hooks,
                output_format,
            };

            loop_runner::run_loop(loop_config, cancel).await?;
        }
        Some(Commands::Autoresearch {
            benchmark,
            metric,
            direction,
            metric_regex,
            checks,
            max_iterations,
            sleep,
            objective,
            benchmark_timeout,
            model,
            thinking,
            api_key,
            directory,
            output_format,
        }) => {
            let cwd = match directory {
                Some(dir) => std::fs::canonicalize(&dir)
                    .with_context(|| format!("Invalid directory: {}", dir.display()))?,
                None => std::env::current_dir().context("Failed to get current directory")?,
            };

            let project_config = load_project_config(&cwd);

            let model_id = model
                .or(project_config.model.clone())
                .unwrap_or_else(|| "claude-sonnet".into());
            let thinking_str = thinking.unwrap_or_else(|| {
                project_config
                    .thinking
                    .map(|t| format!("{:?}", t).to_lowercase())
                    .unwrap_or_else(|| "off".into())
            });
            let thinking_level = parse_thinking(&thinking_str);

            let registry = ModelRegistry::load();
            let model_config = resolve_model_config(&model_id, &registry, thinking_level);
            let model_config = llama_ux::prepare_with_ux(model_config).await?;

            let api_key = match api_key {
                Some(key) => key,
                None => {
                    ModelRegistry::resolve_api_key(&model_config).map_err(|e| anyhow::anyhow!(e))?
                }
            };

            let model = ModelRegistry::to_model(&model_config);
            let tools = build_tools(&cwd, &project_config.allowed_tools, &project_config);

            let autoresearch_system_append = "\n\n## Autoresearch Mode\n\
                You are in autoresearch mode -- an autonomous optimization loop.\n\
                Your goal: improve a measurable metric through iterative code changes.\n\n\
                Rules:\n\
                - Do NOT run git commit, git add, git push, or any git write operations\n\
                - Do NOT run the benchmark command yourself\n\
                - Implement ONE targeted change per iteration\n\
                - Update autoresearch.md with your strategy notes\n\
                - If all viable optimizations are exhausted, write \"EXHAUSTED\" to .stop";

            let system_prompt = build_system_prompt(&cwd, &project_config, Some(autoresearch_system_append));

            let cancel = CancellationToken::new();
            let cancel_clone = cancel.clone();
            tokio::spawn(async move {
                tokio::signal::ctrl_c().await.ok();
                eprintln!("\nInterrupted, cancelling...");
                cancel_clone.cancel();
            });

            let post_tools_hooks: Vec<Arc<dyn rho_core::hooks::PostToolsHook>> = project_config
                .post_tools_hooks
                .iter()
                .map(|h| -> Arc<dyn rho_core::hooks::PostToolsHook> {
                    Arc::new(rho_core::hooks::ShellCommandHook {
                        config: h.clone(),
                        cwd: cwd.clone(),
                    })
                })
                .collect();

            let compiled_regex = match metric_regex {
                Some(ref pattern) => Some(
                    regex::Regex::new(pattern)
                        .with_context(|| format!("Invalid metric regex: {}", pattern))?,
                ),
                None => None,
            };

            let output_format = OutputFormat::parse(&output_format);

            let ar_config = autoresearch::AutoresearchConfig {
                benchmark_command: benchmark,
                metric_name: metric,
                direction: autoresearch::MetricDirection::from_str(&direction),
                metric_regex: compiled_regex,
                checks_command: checks,
                max_iterations,
                sleep_between: Duration::from_secs(sleep),
                objective,
                benchmark_timeout: Duration::from_secs(benchmark_timeout),
                model,
                api_key,
                system_prompt,
                tools,
                thinking: thinking_level,
                cwd,
                stream_fn: rho_provider::stream_fn_for_model(&model_config),
                post_tools_hooks,
                output_format,
            };

            autoresearch::run_autoresearch(ar_config, cancel).await?;
        }
        Some(Commands::Models { command }) => {
            match command {
                ModelsCommands::List => {
                    let registry = ModelRegistry::load();
                    let models = registry.list();
                    eprintln!("{:<24} {:<16} {:<32} {}", "ID", "PROVIDER", "MODEL ID", "STATUS");
                    eprintln!("{}", "-".repeat(90));
                    for m in models {
                        let status = if ModelRegistry::resolve_api_key(m).is_ok() {
                            "\x1b[32mavailable\x1b[0m"
                        } else {
                            "\x1b[31mno key\x1b[0m"
                        };
                        let provider = match m.provider {
                            ProviderType::Anthropic => "anthropic",
                            ProviderType::OpenAi => "openai",
                            ProviderType::XaiResponses => "xai-responses",
                            ProviderType::LlamaCpp => "llama-cpp",
                        };
                        eprintln!("{:<24} {:<16} {:<32} {}", m.id, provider, m.model_id, status);
                    }
                }
                ModelsCommands::Setup { model } => {
                    use rho_core::models::{local_model_catalog, LocalModelTemplate};

                    let catalog = local_model_catalog();

                    let template: LocalModelTemplate = match model {
                        Some(ref name) => {
                            match catalog.iter().find(|t| t.id == name || t.ollama_tag == name) {
                                Some(t) => t.clone(),
                                None => {
                                    eprintln!("Unknown model '{}'. Available models:", name);
                                    for t in &catalog {
                                        eprintln!("  {:<20} {}", t.id, t.display_name);
                                    }
                                    std::process::exit(1);
                                }
                            }
                        }
                        None => {
                            eprintln!("Available local models (all fit 24–34 GB Apple Silicon):\n");
                            eprintln!("  {:<28} {:<10} {}", "ID", "SIZE", "DESCRIPTION");
                            eprintln!("  {}", "-".repeat(72));
                            for t in &catalog {
                                eprintln!("  {:<28} {:<10} {}", t.id, t.size_hint, t.display_name);
                            }
                            eprintln!("\nUsage: rho models setup <model-id>");
                            eprintln!("Example: rho models setup gemma-4-e4b");
                            std::process::exit(0);
                        }
                    };

                    // Check Ollama is available
                    eprintln!("Setting up {} via Ollama...\n", template.display_name);

                    let ollama_check = std::process::Command::new("ollama")
                        .arg("--version")
                        .output();
                    match ollama_check {
                        Ok(out) if out.status.success() => {
                            let ver = String::from_utf8_lossy(&out.stdout);
                            eprintln!("  Ollama: {}", ver.trim());
                        }
                        _ => {
                            eprintln!("Error: Ollama is not installed.");
                            eprintln!("Install it from https://ollama.com/download");
                            std::process::exit(1);
                        }
                    }

                    // Pull the model
                    eprintln!("  Pulling {} ({}), this may take a while...\n",
                        template.ollama_tag, template.size_hint);

                    let pull_status = std::process::Command::new("ollama")
                        .args(["pull", template.ollama_tag])
                        .status();
                    match pull_status {
                        Ok(s) if s.success() => {
                            eprintln!("\n  Pull complete.");
                        }
                        Ok(s) => {
                            eprintln!("\n  Ollama pull failed (exit {}).", s.code().unwrap_or(-1));
                            eprintln!("  Make sure Ollama is running: ollama serve");
                            std::process::exit(1);
                        }
                        Err(e) => {
                            eprintln!("\n  Failed to run ollama: {e}");
                            std::process::exit(1);
                        }
                    }

                    // Write to ~/.rho/models.toml
                    let config = template.to_model_config();
                    match ModelRegistry::save_model_to_config(&config) {
                        Ok(()) => {
                            eprintln!("  Added '{}' to ~/.rho/models.toml\n", template.id);
                            eprintln!("You can now use it with:");
                            eprintln!("  rho --model {} \"your prompt\"", template.id);
                        }
                        Err(e) => {
                            eprintln!("  Failed to save config: {e}");
                            std::process::exit(1);
                        }
                    }
                }
                ModelsCommands::Running => {
                    use rho_provider::llama_cpp::LlamaCppManager;
                    let running = LlamaCppManager::list_running()
                        .context("failed to scan ~/.rho/run")?;
                    if running.is_empty() {
                        eprintln!("No llama.cpp servers are currently running.");
                    } else {
                        eprintln!(
                            "{:<24} {:<8} {:<8} {:<14} {}",
                            "ID", "PORT", "PID", "UPTIME", "GGUF"
                        );
                        eprintln!("{}", "-".repeat(96));
                        for m in running {
                            let uptime = SystemTime::now()
                                .duration_since(m.started_at)
                                .unwrap_or_default();
                            eprintln!(
                                "{:<24} {:<8} {:<8} {:<14} {}",
                                m.id,
                                m.port,
                                m.pid,
                                format_uptime(uptime),
                                m.gguf_path.display()
                            );
                        }
                    }
                }
                ModelsCommands::Stop { id, all } => {
                    use rho_provider::llama_cpp::LlamaCppManager;
                    if all {
                        let stopped = LlamaCppManager::stop_all()
                            .context("failed to stop all llama.cpp servers")?;
                        if stopped.is_empty() {
                            eprintln!("No running servers to stop.");
                        } else {
                            for id in &stopped {
                                eprintln!("\x1b[32m✓\x1b[0m Stopped {}", id);
                            }
                        }
                    } else if let Some(id) = id {
                        match LlamaCppManager::stop(&id)
                            .with_context(|| format!("failed to stop {}", id))?
                        {
                            true => eprintln!("\x1b[32m✓\x1b[0m Stopped {}", id),
                            false => eprintln!("No running server for '{}'.", id),
                        }
                    } else {
                        eprintln!("Specify a model id or --all");
                        std::process::exit(2);
                    }
                }
                ModelsCommands::Warmup { id } => {
                    let registry = ModelRegistry::load();
                    let config = registry
                        .get(&id)
                        .with_context(|| format!("model '{}' not in ~/.rho/models.toml", id))?
                        .clone();
                    if config.provider != ProviderType::LlamaCpp {
                        anyhow::bail!(
                            "model '{}' is not a llama-cpp model (provider={:?}); warmup \
                             only applies to llama-cpp",
                            id,
                            config.provider
                        );
                    }
                    // Goes through the same UX layer as agent calls — spinner on
                    // fresh spawn, cache-hit line if already running.
                    let _ = llama_ux::prepare_with_ux(config).await?;
                }
            }
        }
        None => {
            // Single-shot mode
            let cwd = match &cli.directory {
                Some(dir) => std::fs::canonicalize(dir)
                    .with_context(|| format!("Invalid directory: {}", dir.display()))?,
                None => std::env::current_dir().context("Failed to get current directory")?,
            };

            let project_config = load_project_config(&cwd);

            // Resolve optional prompt: --prompt-file takes precedence, then positional.
            // If no prompt is provided with --resume, we'll enter interactive mode.
            let prompt_from_cli = if let Some(ref prompt_file) = cli.prompt_file {
                Some(std::fs::read_to_string(prompt_file).with_context(|| {
                    format!("Failed to read prompt file: {}", prompt_file.display())
                })?)
            } else if let Some(ref p) = cli.prompt {
                Some(p.clone())
            } else {
                None
            };

            let model_id = project_config
                .model
                .as_deref()
                .unwrap_or(&cli.model)
                .to_string();
            let thinking = project_config
                .thinking
                .unwrap_or_else(|| parse_thinking(&cli.thinking));

            let registry = ModelRegistry::load();
            let model_config = resolve_model_config(&model_id, &registry, thinking);
            let model_config = llama_ux::prepare_with_ux(model_config).await?;

            let api_key = match &cli.api_key {
                Some(key) => key.clone(),
                None => {
                    ModelRegistry::resolve_api_key(&model_config).map_err(|e| anyhow::anyhow!(e))?
                }
            };

            let model = ModelRegistry::to_model(&model_config);

            // Merge tool restrictions: CLI flag overrides config
            let allowed_tools = cli.tools.or(project_config.allowed_tools.clone());
            let tools = build_tools(&cwd, &allowed_tools, &project_config);

            let system_prompt =
                build_system_prompt(&cwd, &project_config, cli.system_append.as_deref());

            let running_under_scud = std::env::var("SCUD_TASK_ID").is_ok();
            let store_path = session_db_path(&cwd);
            let session_store = match SessionStore::open(&store_path) {
                Ok(store) => Some(Arc::new(store)),
                Err(e) => {
                    if cli.resume.is_some() || running_under_scud {
                        anyhow::bail!(
                            "Failed to open session store at {}: {}",
                            store_path.display(),
                            e
                        );
                    }
                    eprintln!(
                        "[warn] session persistence disabled ({}: {})",
                        store_path.display(),
                        e
                    );
                    None
                }
            };

            let mut session_id = cli.resume.clone();
            let mut history: Vec<Message> = Vec::new();

            // Load pre-seeded messages from context file (cache-optimized subagent forking)
            if let Some(ref context_path) = cli.context_file {
                let content = std::fs::read_to_string(context_path)
                    .with_context(|| format!("Failed to read context file: {}", context_path.display()))?;
                history = serde_json::from_str(&content)
                    .with_context(|| "Failed to parse context file as JSON array of messages")?;
            }

            if let Some(store) = session_store.as_ref() {
                if let Some(ref existing_id) = session_id {
                    if !store
                        .session_exists(existing_id)
                        .with_context(|| "Failed to verify existing session ID")?
                    {
                        anyhow::bail!(
                            "Session '{}' not found in {}",
                            existing_id,
                            store_path.display()
                        );
                    }
                    history = store
                        .load_messages(existing_id)
                        .with_context(|| format!("Failed to load session '{}'", existing_id))?;
                } else {
                    let session = store
                        .create_session(&model_id, &cwd)
                        .with_context(|| "Failed to create session")?;
                    session_id = Some(session.id);
                }
            } else if cli.resume.is_some() {
                anyhow::bail!("--resume requires a writable session store");
            }

            let output_format = OutputFormat::parse(&cli.output_format);

            let mut event_handler_cfg = session_store.clone().map(|store| EventHandlerConfig {
                session_store: Some(store as Arc<dyn SessionPersistence>),
                session_id: session_id.clone(),
                model_id: model_id.clone(),
                cwd: cwd.clone(),
            });
            let mut last_reported_session_id = None;
            if let Some(ref id) = session_id {
                match output_format {
                    OutputFormat::Text => {
                        eprintln!("[session:{}]", id);
                    }
                    OutputFormat::StreamJson => {
                        println!("{}", serde_json::json!({
                            "type": "session",
                            "session_id": id
                        }));
                    }
                }
                last_reported_session_id = Some(id.clone());
            }

            let cancel = CancellationToken::new();
            let cancel_clone = cancel.clone();
            tokio::spawn(async move {
                tokio::signal::ctrl_c().await.ok();
                eprintln!("\nInterrupted, cancelling...");
                cancel_clone.cancel();
            });

            let post_tools_hooks: Vec<Arc<dyn rho_core::hooks::PostToolsHook>> = project_config
                .post_tools_hooks
                .iter()
                .map(|h| -> Arc<dyn rho_core::hooks::PostToolsHook> {
                    Arc::new(rho_core::hooks::ShellCommandHook {
                        config: h.clone(),
                        cwd: cwd.clone(),
                    })
                })
                .collect();

            if prompt_from_cli.is_none() && cli.resume.is_some() {
                let resumed = session_id.as_deref().unwrap_or("unknown");
                println!("Resumed session {}. Type /exit to quit.", resumed);
                let mut conversation = history;
                loop {
                    if cancel.is_cancelled() {
                        break;
                    }
                    let Some(input) = read_prompt_line()? else {
                        break;
                    };
                    if input.is_empty() {
                        continue;
                    }
                    if input == "/exit" || input == "/quit" {
                        break;
                    }
                    let mut turn_messages = conversation.clone();
                    turn_messages.push(Message::User {
                        content: UserContent::Text(input),
                        timestamp: now_ms(),
                    });
                    conversation = run_agent_turn(
                        turn_messages,
                        model.clone(),
                        api_key.clone(),
                        system_prompt.clone(),
                        tools.clone(),
                        thinking.clone(),
                        &model_config,
                        project_config.compact_threshold,
                        cli.show_thinking,
                        cancel.clone(),
                        &mut event_handler_cfg,
                        &mut last_reported_session_id,
                        output_format,
                        post_tools_hooks.clone(),
                    )
                    .await;
                }
            } else {
                let prompt = match prompt_from_cli {
                    Some(p) => p,
                    None => {
                        anyhow::bail!(
                            "No prompt provided. Use a positional argument, --prompt-file, or --resume."
                        );
                    }
                };
                let mut messages = history;
                messages.push(Message::User {
                    content: UserContent::Text(prompt),
                    timestamp: now_ms(),
                });
                let _ = run_agent_turn(
                    messages,
                    model,
                    api_key,
                    system_prompt,
                    tools,
                    thinking.clone(),
                    &model_config,
                    project_config.compact_threshold,
                    cli.show_thinking,
                    cancel,
                    &mut event_handler_cfg,
                    &mut last_reported_session_id,
                    output_format,
                    post_tools_hooks,
                )
                .await;
            }
        }
    }

    Ok(())
}
