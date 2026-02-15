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

    /// Model ID to use
    #[arg(long, default_value = "claude-sonnet-4-5-20250929")]
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

When editing files, first read them to get LINE:HASH references, then use edit with those anchors. \
For new files, use write. For small changes, use edit. For running tests or builds, use bash.";

fn build_tools(cwd: &PathBuf, allowed: &Option<Vec<String>>) -> Vec<Arc<dyn AgentTool>> {
    let all_tools: Vec<Arc<dyn AgentTool>> = vec![
        Arc::new(rho_tools::read::ReadTool::with_cwd(cwd.clone())),
        Arc::new(rho_tools::write::WriteTool::with_cwd(cwd.clone())),
        Arc::new(rho_tools::edit::EditTool::with_cwd(cwd.clone())),
        Arc::new(rho_tools::bash::BashTool::new(cwd.clone())),
        Arc::new(rho_tools::grep::GrepTool::new(cwd.clone())),
        Arc::new(rho_tools::find::FindTool::new(cwd.clone())),
        Arc::new(rho_tools::task::TaskTool::new(cwd.clone())),
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

fn build_model(model_id: &str, thinking: ThinkingLevel) -> Model {
    Model {
        id: model_id.to_string(),
        name: model_id.to_string(),
        provider: "anthropic".into(),
        base_url: String::new(),
        reasoning: model_id.contains("opus") || thinking != ThinkingLevel::Off,
        context_window: 200_000,
        max_tokens: if thinking != ThinkingLevel::Off {
            16_384
        } else {
            8_192
        },
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

    // Add skills
    let skill_dirs = rho_core::skills::default_skill_dirs(cwd);
    let skills = rho_core::skills::discover_skills(&skill_dirs);
    if !skills.is_empty() {
        prompt.push_str("\n\n");
        prompt.push_str(&rho_core::skills::format_skills_prompt(&skills));
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
                .unwrap_or_else(|| "claude-sonnet-4-5-20250929".into());
            let thinking_str = thinking.unwrap_or_else(|| {
                project_config
                    .thinking
                    .map(|t| format!("{:?}", t).to_lowercase())
                    .unwrap_or_else(|| "off".into())
            });
            let thinking_level = parse_thinking(&thinking_str);

            let api_key = match api_key {
                Some(key) => key,
                None => anthropic_auth::get_token().context("Failed to get API key")?,
            };

            let model = build_model(&model_id, thinking_level);

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
                stream_fn: rho_provider::anthropic_stream_fn(),
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
                .unwrap_or(&cli.model);
            let thinking = project_config
                .thinking
                .unwrap_or_else(|| parse_thinking(&cli.thinking));

            let api_key = match &cli.api_key {
                Some(key) => key.clone(),
                None => anthropic_auth::get_token().context("Failed to get API key")?,
            };

            let model = build_model(model_id, thinking);

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
                stream_fn: rho_provider::anthropic_stream_fn(),
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
