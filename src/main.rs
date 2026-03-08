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

mod loop_runner;

#[derive(Parser)]
#[command(name = "rho", about = "AI coding agent with file tools")]
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

    /// Load all tools (including grep, find, web_fetch, web_search)
    #[arg(long)]
    all_tools: bool,

    /// Append to system prompt
    #[arg(long)]
    system_append: Option<String>,

    /// Branch from an existing session (use with --resume)
    #[arg(long)]
    branch: bool,

    /// Enable planning mode (produce plan before executing)
    #[arg(long)]
    plan: bool,
}

#[derive(Subcommand)]
enum Commands {
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
    },

    /// Start HTTP server mode (agent as a service)
    Serve {
        /// Port to listen on
        #[arg(long, default_value_t = 7890)]
        port: u16,

        /// Bind address
        #[arg(long, default_value = "127.0.0.1")]
        bind: String,

        /// Disable bearer token authentication
        #[arg(long)]
        no_auth: bool,

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

        /// Load all tools (including grep, find, web_fetch, web_search)
        #[arg(long)]
        all_tools: bool,
    },
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
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
You are a coding assistant with tools for reading, editing, and running commands.

When editing files, first read them to get LINE:HASH references, then use edit with those anchors. \
For new files, use write. For small changes, use edit. For running tests, builds, file search (rg, fd), \
web requests (curl), or any CLI operation, use bash.";

/// Default 5 core tools — the model can use bash for grep/find/web operations.
const DEFAULT_TOOLS: &[&str] = &["read", "write", "edit", "bash", "task"];

fn build_tools(cwd: &PathBuf, allowed: &Option<Vec<String>>, all_tools_flag: bool, auto_commit: bool) -> Vec<Arc<dyn AgentTool>> {
    let every_tool: Vec<Arc<dyn AgentTool>> = vec![
        Arc::new(rho_tools::read::ReadTool::with_cwd(cwd.clone())),
        Arc::new(rho_tools::write::WriteTool::with_cwd(cwd.clone()).with_auto_commit(auto_commit)),
        Arc::new(rho_tools::edit::EditTool::with_cwd(cwd.clone()).with_auto_commit(auto_commit)),
        Arc::new(rho_tools::bash::BashTool::new(cwd.clone())),
        Arc::new(rho_tools::grep::GrepTool::new(cwd.clone())),
        Arc::new(rho_tools::find::FindTool::new(cwd.clone())),
        Arc::new(rho_tools::task::TaskTool::new(cwd.clone())),
        Arc::new(rho_tools::web_fetch::WebFetchTool::new()),
        Arc::new(rho_tools::web_search::WebSearchTool::new()),
    ];

    if let Some(ref allowed) = allowed {
        // Explicit tool list from --tools or config
        every_tool
            .into_iter()
            .filter(|t| allowed.iter().any(|a| a == t.name()))
            .collect()
    } else if all_tools_flag {
        // --all-tools: load everything
        every_tool
    } else {
        // Default: 5 core tools
        every_tool
            .into_iter()
            .filter(|t| DEFAULT_TOOLS.contains(&t.name()))
            .collect()
    }
}

/// Resolve a model name (registry ID or raw model ID) to a ModelConfig.
/// For raw IDs not in the registry, a synthetic config is created.
fn resolve_model_config(
    model_arg: &str,
    registry: &ModelRegistry,
    thinking: ThinkingLevel,
) -> ModelConfig {
    if let Some(config) = registry.get(model_arg) {
        return config.clone();
    }

    // Fallback: treat as raw model ID, infer provider from name
    let provider = if model_arg.contains("claude") {
        ProviderType::Anthropic
    } else {
        ProviderType::OpenAi
    };

    ModelConfig {
        id: model_arg.to_string(),
        provider: provider.clone(),
        model_id: model_arg.to_string(),
        base_url: String::new(),
        api_key_env: match provider {
            ProviderType::Anthropic => Some("ANTHROPIC_API_KEY".into()),
            ProviderType::OpenAi => Some("OPENAI_API_KEY".into()),
        },
        context_window: 200_000,
        max_tokens: if thinking != ThinkingLevel::Off {
            16_384
        } else {
            8_192
        },
        thinking: model_arg.contains("opus") || thinking != ThinkingLevel::Off,
        server_tools: None,
    }
}

fn build_system_prompt(
    cwd: &PathBuf,
    config: &rho_core::config::ProjectConfig,
    system_append: Option<&str>,
    tools: &[Arc<dyn AgentTool>],
) -> String {
    let base = config.system_prompt.as_deref().unwrap_or(SYSTEM_PROMPT);

    let mut prompt = base.to_string();

    // Add brief tool descriptions
    if !tools.is_empty() {
        prompt.push_str("\n\nAvailable tools:\n");
        for tool in tools {
            prompt.push_str(&format!("- {}: {}\n", tool.name(), tool.brief_description()));
        }
    }

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
    planning: bool,
    cancel: CancellationToken,
    session_handler: &mut Option<EventHandlerConfig>,
    last_reported_session_id: &mut Option<String>,
) -> Vec<Message> {
    let transform_messages = compact_threshold.map(compaction::make_compaction_transform);
    let config = AgentLoopConfig {
        model,
        api_key,
        system_prompt,
        tools,
        thinking,
        max_tokens: None,
        stream_fn: rho_provider::stream_fn_for_model(model_config),
        planning,
        get_steering_messages: None,
        get_follow_up_messages: None,
        transform_messages,
    };

    let mut consumer = agent_loop(messages.clone(), config, cancel);
    let mut stdout = std::io::stdout();
    let mut final_messages = messages;

    while let Some(event) = consumer.next().await {
        if let Some(handler) = session_handler.as_mut() {
            if let Some(update) = handle_event(&event, handler) {
                if let Some(session_id) = update.session_id {
                    if last_reported_session_id.as_deref() != Some(session_id.as_str()) {
                        eprintln!("[session:{}]", session_id);
                        *last_reported_session_id = Some(session_id);
                    }
                }
            }
        }

        match event {
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
            AgentEvent::PlanProduced { plan } => {
                eprintln!("\n[plan] Plan produced. Review the plan above and respond with:");
                eprintln!("  - \"approved\" to proceed with execution");
                eprintln!("  - modifications to adjust the plan");
                eprintln!("  - \"reject\" to cancel");
                let _ = plan; // plan text already printed via TextDelta events
            }
            AgentEvent::AgentEnd { messages } => {
                final_messages = messages;
                println!();
            }
            _ => {}
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
        Some(Commands::Loop {
            mode,
            plan,
            max_iterations,
            sleep,
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
            let tools = build_tools(&cwd, &default_tools, true, project_config.auto_commit);
            let system_prompt = build_system_prompt(&cwd, &project_config, None, &tools);

            let cancel = CancellationToken::new();
            let cancel_clone = cancel.clone();
            tokio::spawn(async move {
                tokio::signal::ctrl_c().await.ok();
                eprintln!("\nInterrupted, cancelling...");
                cancel_clone.cancel();
            });

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
            };

            loop_runner::run_loop(loop_config, cancel).await?;
        }
        Some(Commands::Serve {
            port,
            bind,
            no_auth,
            model,
            thinking,
            api_key,
            directory,
            all_tools,
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

            let api_key = match api_key {
                Some(key) => key,
                None => {
                    ModelRegistry::resolve_api_key(&model_config).map_err(|e| anyhow::anyhow!(e))?
                }
            };

            let tools = build_tools(&cwd, &project_config.allowed_tools, all_tools, project_config.auto_commit);
            let system_prompt = build_system_prompt(&cwd, &project_config, None, &tools);

            // Generate auth token
            let auth_token = if no_auth {
                None
            } else {
                Some(format!("rho_sk_{}", uuid::Uuid::new_v4().to_string().replace("-", "")))
            };

            let mut builder = rho_server::ServerBuilder::new(
                project_config,
                registry,
                model_config,
                api_key,
                cwd,
                tools,
                system_prompt,
            );
            if let Some(ref token) = auth_token {
                builder = builder.with_auth_token(token.clone());
            }
            let app = builder.build();

            let addr = format!("{}:{}", bind, port);
            let listener = tokio::net::TcpListener::bind(&addr)
                .await
                .with_context(|| format!("Failed to bind to {}", addr))?;

            eprintln!("[server] listening on http://{}", addr);
            if let Some(ref token) = auth_token {
                eprintln!("[server] auth token: {}", token);
            } else {
                eprintln!("[server] authentication disabled (--no-auth)");
            }
            if bind == "0.0.0.0" {
                eprintln!("[server] WARNING: binding to all interfaces — ensure TLS via reverse proxy or SSH tunnel");
            }

            axum::serve(listener, app)
                .await
                .context("Server error")?;
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

            let api_key = match &cli.api_key {
                Some(key) => key.clone(),
                None => {
                    ModelRegistry::resolve_api_key(&model_config).map_err(|e| anyhow::anyhow!(e))?
                }
            };

            let model = ModelRegistry::to_model(&model_config);

            // Merge tool restrictions: CLI flag overrides config
            let allowed_tools = cli.tools.or(project_config.allowed_tools.clone());
            let tools = build_tools(&cwd, &allowed_tools, cli.all_tools, project_config.auto_commit);
            let planning = cli.plan || project_config.planning;

            let system_prompt =
                build_system_prompt(&cwd, &project_config, cli.system_append.as_deref(), &tools);

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
                    if cli.branch {
                        // Branch: create a new session that inherits parent messages
                        let new_id = store
                            .branch_session(existing_id)
                            .with_context(|| {
                                format!("Failed to branch session '{}'", existing_id)
                            })?;
                        eprintln!(
                            "[branched session {} from {}]",
                            new_id, existing_id
                        );
                        history = store
                            .load_messages(&new_id)
                            .with_context(|| {
                                format!("Failed to load branched session '{}'", new_id)
                            })?;
                        session_id = Some(new_id);
                    } else {
                        history = store
                            .load_messages(existing_id)
                            .with_context(|| {
                                format!("Failed to load session '{}'", existing_id)
                            })?;
                    }
                } else {
                    if cli.branch {
                        anyhow::bail!("--branch requires --resume <session-id> to specify which session to branch from");
                    }
                    let session = store
                        .create_session(&model_id, &cwd)
                        .with_context(|| "Failed to create session")?;
                    session_id = Some(session.id);
                }
            } else if cli.resume.is_some() {
                anyhow::bail!("--resume requires a writable session store");
            } else if cli.branch {
                anyhow::bail!("--branch requires --resume <session-id> to specify which session to branch from");
            }

            let mut event_handler_cfg = session_store.clone().map(|store| EventHandlerConfig {
                session_store: Some(store as Arc<dyn SessionPersistence>),
                session_id: session_id.clone(),
                model_id: model_id.clone(),
                cwd: cwd.clone(),
            });
            let mut last_reported_session_id = None;
            if let Some(ref id) = session_id {
                eprintln!("[session:{}]", id);
                last_reported_session_id = Some(id.clone());
            }

            let cancel = CancellationToken::new();
            let cancel_clone = cancel.clone();
            tokio::spawn(async move {
                tokio::signal::ctrl_c().await.ok();
                eprintln!("\nInterrupted, cancelling...");
                cancel_clone.cancel();
            });

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
                        planning,
                        cancel.clone(),
                        &mut event_handler_cfg,
                        &mut last_reported_session_id,
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
                    planning,
                    cancel,
                    &mut event_handler_cfg,
                    &mut last_reported_session_id,
                )
                .await;
            }
        }
    }

    Ok(())
}
