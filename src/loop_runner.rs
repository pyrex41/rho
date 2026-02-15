use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use rho_core::agent_loop::{agent_loop, AgentLoopConfig};
use rho_core::tool::AgentTool;
use rho_core::types::*;

pub struct LoopConfig {
    pub mode: LoopMode,
    pub plan_path: PathBuf,
    pub max_iterations: usize,
    pub sleep_between: Duration,
    pub model: Model,
    pub api_key: String,
    pub system_prompt: String,
    pub tools: Vec<Arc<dyn AgentTool>>,
    pub thinking: ThinkingLevel,
    pub validation_commands: Vec<String>,
    pub cwd: PathBuf,
    pub stream_fn: rho_core::provider_types::StreamFn,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LoopMode {
    Build,
    Plan,
}

impl LoopMode {
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "plan" => LoopMode::Plan,
            _ => LoopMode::Build,
        }
    }
}

const PLAN_PROMPT_TEMPLATE: &str = "\
Study the codebase and any specs/ directory. Create or update {plan_path} with a detailed \
implementation plan. Mark tasks as [TODO]. When updating an existing plan, preserve [DONE] \
tasks and update [TODO]/[CURRENT] tasks based on your analysis. Then exit.";

const BUILD_PROMPT_TEMPLATE: &str = "\
Read {plan_path}. Find the first task marked [TODO] or [CURRENT]. Mark it [CURRENT] and \
implement it fully. After implementation:
1. Run any validation commands to verify correctness
2. Update the task status to [DONE] in {plan_path}
3. Commit the changes with a descriptive message
4. If all tasks are [DONE], write \"All tasks complete\" to .stop

Then exit.";

pub async fn run_loop(config: LoopConfig, cancel: CancellationToken) -> anyhow::Result<()> {
    let plan_path_str = config.plan_path.display().to_string();

    for iteration in 1..=config.max_iterations {
        if cancel.is_cancelled() {
            eprintln!("[loop] Cancelled");
            break;
        }

        // Check .stop file
        let stop_file = config.cwd.join(".stop");
        if stop_file.exists() {
            let reason = std::fs::read_to_string(&stop_file).unwrap_or_default();
            let _ = std::fs::remove_file(&stop_file);
            eprintln!("[loop] Stopped: {}", reason.trim());
            break;
        }

        eprintln!(
            "\n[loop] === Iteration {}/{} ({:?} mode) ===",
            iteration, config.max_iterations, config.mode
        );

        // Build prompt based on mode
        let prompt = match config.mode {
            LoopMode::Plan => PLAN_PROMPT_TEMPLATE.replace("{plan_path}", &plan_path_str),
            LoopMode::Build => BUILD_PROMPT_TEMPLATE.replace("{plan_path}", &plan_path_str),
        };

        let prompts = vec![Message::User {
            content: UserContent::Text(prompt),
            timestamp: now_ms(),
        }];

        let loop_config = AgentLoopConfig {
            model: config.model.clone(),
            api_key: config.api_key.clone(),
            system_prompt: config.system_prompt.clone(),
            tools: config.tools.clone(),
            thinking: config.thinking,
            max_tokens: None,
            stream_fn: config.stream_fn.clone(),
            get_steering_messages: None,
            get_follow_up_messages: None,
            transform_messages: None,
        };

        let mut consumer = agent_loop(prompts, loop_config, cancel.clone());

        // Consume all events, print to stderr
        while let Some(event) = consumer.next().await {
            match event {
                AgentEvent::MessageUpdate { event, .. } => {
                    if let AssistantStreamEvent::TextDelta { delta, .. } = event {
                        eprint!("{}", delta);
                    }
                }
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
                    ..
                } => {
                    if is_error {
                        eprintln!("[tool:{}] ERROR", tool_name);
                    } else {
                        eprintln!("[tool:{}] done", tool_name);
                    }
                }
                AgentEvent::AgentEnd { .. } => {
                    eprintln!();
                }
                _ => {}
            }
        }

        // Check .stop file again after agent exits
        let stop_file = config.cwd.join(".stop");
        if stop_file.exists() {
            let reason = std::fs::read_to_string(&stop_file).unwrap_or_default();
            let _ = std::fs::remove_file(&stop_file);
            eprintln!("[loop] Stopped after iteration: {}", reason.trim());
            break;
        }

        // Run validation commands
        if !config.validation_commands.is_empty() {
            eprintln!("[loop] Running validation...");
            for cmd in &config.validation_commands {
                eprintln!("[validate] $ {}", cmd);
                let output = tokio::process::Command::new("sh")
                    .arg("-c")
                    .arg(cmd)
                    .current_dir(&config.cwd)
                    .output()
                    .await;

                match output {
                    Ok(out) => {
                        if !out.status.success() {
                            let stderr = String::from_utf8_lossy(&out.stderr);
                            let stdout = String::from_utf8_lossy(&out.stdout);
                            eprintln!("[validate] FAILED: {}", cmd);
                            if !stdout.is_empty() {
                                eprintln!("{}", stdout);
                            }
                            if !stderr.is_empty() {
                                eprintln!("{}", stderr);
                            }
                            eprintln!("[loop] Validation failed, stopping loop");
                            return Ok(());
                        }
                        eprintln!("[validate] ok");
                    }
                    Err(e) => {
                        eprintln!("[validate] Failed to run '{}': {}", cmd, e);
                        return Ok(());
                    }
                }
            }
        }

        // Sleep between iterations
        if iteration < config.max_iterations {
            eprintln!(
                "[loop] Sleeping {}s before next iteration...",
                config.sleep_between.as_secs()
            );
            tokio::select! {
                _ = tokio::time::sleep(config.sleep_between) => {},
                _ = cancel.cancelled() => {
                    eprintln!("[loop] Cancelled during sleep");
                    break;
                }
            }
        }
    }

    eprintln!("[loop] Done");
    Ok(())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
