use std::io::Write;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use clap::Parser;
use tokio_util::sync::CancellationToken;

use rho_core::agent_loop::{agent_loop, AgentLoopConfig};
use rho_core::tool::AgentTool;
use rho_core::types::*;

#[derive(Parser)]
#[command(name = "rho", about = "Minimal AI agent")]
struct Cli {
    /// The prompt to send to the agent
    prompt: String,

    /// Model ID to use
    #[arg(long, default_value = "claude-sonnet-4-5-20250929")]
    model: String,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
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

    let api_key = anthropic_auth::get_token().context("Failed to get API key")?;

    let model = Model {
        id: cli.model.clone(),
        name: cli.model.clone(),
        provider: "anthropic".into(),
        base_url: String::new(),
        reasoning: cli.model.contains("opus"),
        context_window: 200_000,
        max_tokens: 16_384,
    };

    let tools: Vec<Arc<dyn AgentTool>> = vec![Arc::new(rho_tools::write::WriteTool::new())];

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

    let system_prompt = "You are a helpful AI assistant. When asked to write files, use the 'write' tool with 'path' and 'content' parameters.".to_string();

    let config = AgentLoopConfig {
        model,
        api_key,
        system_prompt,
        tools,
        thinking: ThinkingLevel::Off,
        max_tokens: None,
        stream_fn: rho_provider::anthropic_stream_fn(),
        get_steering_messages: None,
        get_follow_up_messages: None,
    };

    let mut consumer = agent_loop(prompts, config, cancel);

    let mut stdout = std::io::stdout();

    while let Some(event) = consumer.next().await {
        match event {
            AgentEvent::MessageUpdate { event, .. } => match event {
                AssistantStreamEvent::TextDelta { delta, .. } => {
                    print!("{}", delta);
                    stdout.flush().ok();
                }
                AssistantStreamEvent::ThinkingDelta { delta, .. } => {
                    eprint!("{}", delta);
                    std::io::stderr().flush().ok();
                }
                _ => {}
            },
            AgentEvent::ToolExecutionStart {
                tool_name, args, ..
            } => {
                eprintln!(
                    "\n[tool] {} {}",
                    tool_name,
                    serde_json::to_string(&args).unwrap_or_default()
                );
            }
            AgentEvent::ToolExecutionEnd {
                tool_name,
                is_error,
                result,
                ..
            } => {
                if is_error {
                    eprintln!("[tool] {} ERROR: {:?}", tool_name, result.content);
                } else {
                    eprintln!("[tool] {} done", tool_name);
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
