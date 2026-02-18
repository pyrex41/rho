use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tokio_util::sync::CancellationToken;

use rho_core::agent_loop::{agent_loop, AgentLoopConfig};
use rho_core::compaction;
use rho_core::config::load_project_config;
use rho_core::models::{ModelConfig, ModelRegistry, ProviderType};
use rho_core::tool::AgentTool;
use rho_core::types::*;

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

    /// Restrict available tools (comma-separated names)
    #[arg(long, value_delimiter = ',')]
    tools: Option<Vec<String>>,

    /// Append to system prompt
    #[arg(long)]
    system_append: Option<String>,
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

fn build_tools(cwd: &PathBuf, allowed: &Option<Vec<String>>) -> Vec<Arc<dyn AgentTool>> {
    let all_tools: Vec<Arc<dyn AgentTool>> = vec![
        Arc::new(rho_tools::read::ReadTool::with_cwd(cwd.clone())),
        Arc::new(rho_tools::write::WriteTool::with_cwd(cwd.clone())),
        Arc::new(rho_tools::edit::EditTool::with_cwd(cwd.clone())),
        Arc::new(rho_tools::bash::BashTool::new(cwd.clone())),
        Arc::new(rho_tools::grep::GrepTool::new(cwd.clone())),
        Arc::new(rho_tools::find::FindTool::new(cwd.clone())),
        Arc::new(rho_tools::task::TaskTool::new(cwd.clone())),
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
) -> String {
    let base = config
        .system_prompt
        .as_deref()
        .unwrap_or(SYSTEM_PROMPT);

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
    prompt = prompt.replace("the current year", &format!("the current year ({})", year_str))
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
            prompt.push_str(&format!(
                "  /{} — {}\n",
                cmd.name, cmd.description
            ));
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
                None => ModelRegistry::resolve_api_key(&model_config)
                    .map_err(|e| anyhow::anyhow!(e))?,
            };

            let model = ModelRegistry::to_model(&model_config);

            // Plan mode defaults to read-only tools
            let default_tools = if loop_runner::LoopMode::from_str(&mode) == loop_runner::LoopMode::Plan {
                Some(vec!["read".into(), "grep".into(), "find".into(), "write".into()])
            } else {
                project_config.allowed_tools.clone()
            };
            let tools = build_tools(&cwd, &default_tools);
            let system_prompt = build_system_prompt(&cwd, &project_config, None);

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
        None => {
            // Single-shot mode
            let cwd = match &cli.directory {
                Some(dir) => std::fs::canonicalize(dir)
                    .with_context(|| format!("Invalid directory: {}", dir.display()))?,
                None => std::env::current_dir().context("Failed to get current directory")?,
            };

            let project_config = load_project_config(&cwd);

            // Resolve prompt: --prompt-file takes precedence, then positional, then error
            let prompt = if let Some(ref prompt_file) = cli.prompt_file {
                std::fs::read_to_string(prompt_file)
                    .with_context(|| format!("Failed to read prompt file: {}", prompt_file.display()))?
            } else if let Some(ref p) = cli.prompt {
                p.clone()
            } else {
                anyhow::bail!("No prompt provided. Use a positional argument or --prompt-file.");
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
                None => ModelRegistry::resolve_api_key(&model_config)
                    .map_err(|e| anyhow::anyhow!(e))?,
            };

            let model = ModelRegistry::to_model(&model_config);

            // Merge tool restrictions: CLI flag overrides config
            let allowed_tools = cli.tools.or(project_config.allowed_tools.clone());
            let tools = build_tools(&cwd, &allowed_tools);

            let system_prompt =
                build_system_prompt(&cwd, &project_config, cli.system_append.as_deref());

            let cancel = CancellationToken::new();
            let cancel_clone = cancel.clone();
            tokio::spawn(async move {
                tokio::signal::ctrl_c().await.ok();
                eprintln!("\nInterrupted, cancelling...");
                cancel_clone.cancel();
            });

            let prompts = vec![Message::User {
                content: UserContent::Text(prompt),
                timestamp: now_ms(),
            }];

            // Build compaction transform if configured
            let transform_messages = project_config.compact_threshold.map(|threshold| {
                compaction::make_compaction_transform(threshold)
            });

            let config = AgentLoopConfig {
                model,
                api_key,
                system_prompt,
                tools,
                thinking,
                max_tokens: None,
                stream_fn: rho_provider::stream_fn_for_model(&model_config),
                get_steering_messages: None,
                get_follow_up_messages: None,
                transform_messages,
            };

            let mut consumer = agent_loop(prompts, config, cancel);

            let mut stdout = std::io::stdout();
            let show_thinking = cli.show_thinking;

            while let Some(event) = consumer.next().await {
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
                    AgentEvent::AgentEnd { .. } => {
                        println!();
                    }
                    _ => {}
                }
            }
        }
    }

    Ok(())
}
