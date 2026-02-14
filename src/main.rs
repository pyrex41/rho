use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use clap::Parser;
use tokio_util::sync::CancellationToken;

use rho_core::agent_loop::{agent_loop, AgentLoopConfig};
use rho_core::tool::AgentTool;
use rho_core::types::*;

#[derive(Parser)]
#[command(name = "rho", about = "AI coding agent with file tools")]
struct Cli {
    /// The prompt to send to the agent
    prompt: String,

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

When editing files, first read them to get LINE:HASH references, then use edit with those anchors. \
For new files, use write. For small changes, use edit. For running tests or builds, use bash.";

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

    let cwd = match &cli.directory {
        Some(dir) => std::fs::canonicalize(dir)
            .with_context(|| format!("Invalid directory: {}", dir.display()))?,
        None => std::env::current_dir().context("Failed to get current directory")?,
    };

    let api_key = match &cli.api_key {
        Some(key) => key.clone(),
        None => anthropic_auth::get_token().context("Failed to get API key")?,
    };

    let thinking = parse_thinking(&cli.thinking);

    let model = Model {
        id: cli.model.clone(),
        name: cli.model.clone(),
        provider: "anthropic".into(),
        base_url: String::new(),
        reasoning: cli.model.contains("opus") || thinking != ThinkingLevel::Off,
        context_window: 200_000,
        max_tokens: if thinking != ThinkingLevel::Off {
            16_384
        } else {
            8_192
        },
    };

    let skill_dirs = rho_core::skills::default_skill_dirs(&cwd);
    let skills = rho_core::skills::discover_skills(&skill_dirs);
    let system_prompt = if skills.is_empty() {
        SYSTEM_PROMPT.to_string()
    } else {
        format!(
            "{}\n\n{}",
            SYSTEM_PROMPT,
            rho_core::skills::format_skills_prompt(&skills)
        )
    };

    let tools: Vec<Arc<dyn AgentTool>> = vec![
        Arc::new(rho_tools::read::ReadTool::with_cwd(cwd.clone())),
        Arc::new(rho_tools::write::WriteTool::with_cwd(cwd.clone())),
        Arc::new(rho_tools::edit::EditTool::with_cwd(cwd.clone())),
        Arc::new(rho_tools::bash::BashTool::new(cwd.clone())),
        Arc::new(rho_tools::grep::GrepTool::new(cwd.clone())),
        Arc::new(rho_tools::find::FindTool::new(cwd)),
    ];

    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();

    // Ctrl+C handler
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        eprintln!("\nInterrupted, cancelling...");
        cancel_clone.cancel();
    });

    let prompts = vec![Message::User {
        content: UserContent::Text(cli.prompt),
        timestamp: now_ms(),
    }];

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
            AgentEvent::AgentEnd { .. } => {
                println!();
            }
            _ => {}
        }
    }

    Ok(())
}
