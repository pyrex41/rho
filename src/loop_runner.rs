use std::path::{Path, PathBuf};
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
    pub post_tools_hooks: Vec<Arc<dyn rho_core::hooks::PostToolsHook>>,
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

/// Deterministic context carried between loop iterations.
/// Built from filesystem state, not LLM summaries.
#[derive(Debug, Clone)]
pub struct IterationContext {
    pub task_description: String,
    pub iterations_completed: usize,
    pub last_outcome: IterationOutcome,
    pub completed_items: Vec<String>,
    pub failure_details: Option<String>,
    pub hook_results: Vec<HookSummary>,
    pub last_commit: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum IterationOutcome {
    FirstIteration,
    Success,
    ValidationFailed,
    AgentError,
}

#[derive(Debug, Clone)]
pub struct HookSummary {
    pub name: String,
    pub success: bool,
    pub summary: String,
}

/// Parse completed items from the plan file.
/// Looks for lines containing `[DONE]` and extracts them.
fn parse_completed_items(plan_path: &Path) -> Vec<String> {
    let content = match std::fs::read_to_string(plan_path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    content
        .lines()
        .filter(|line| line.contains("[DONE]"))
        .map(|line| {
            line.trim()
                .trim_start_matches('-')
                .trim_start_matches('*')
                .trim()
                .to_string()
        })
        .collect()
}

/// Get the short hash of the current HEAD commit.
fn get_last_commit(cwd: &Path) -> Option<String> {
    std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(cwd)
        .output()
        .ok()
        .and_then(|out| {
            if out.status.success() {
                Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
            } else {
                None
            }
        })
}

/// Build the iteration prompt from deterministic context.
fn build_iteration_prompt(ctx: &IterationContext, mode: LoopMode, plan_path: &str) -> String {
    let mut prompt = String::new();

    // Task description (always)
    prompt.push_str(&format!("Task: {}\n\n", ctx.task_description));

    // Mode instructions
    match mode {
        LoopMode::Plan => {
            prompt.push_str(&format!(
                "Study the codebase and create/update {} with a detailed implementation plan. \
                 Mark tasks as [TODO]. Preserve [DONE] items.\n\n",
                plan_path
            ));
        }
        LoopMode::Build => {
            prompt.push_str(&format!(
                "Read {}. Find the first [TODO] or [CURRENT] task. Mark it [CURRENT] and implement fully.\n\
                 After implementation: run validation, update task to [DONE], commit changes.\n\
                 If all tasks are [DONE], write \"All tasks complete\" to .stop\n\n",
                plan_path
            ));
        }
    }

    // Previous outcome
    match ctx.last_outcome {
        IterationOutcome::FirstIteration => {}
        IterationOutcome::Success => {
            prompt.push_str(&format!(
                "Previous iteration {} completed successfully.\n",
                ctx.iterations_completed
            ));
        }
        IterationOutcome::ValidationFailed => {
            prompt.push_str(&format!(
                "Previous iteration {} FAILED validation.\n",
                ctx.iterations_completed
            ));
            if let Some(ref details) = ctx.failure_details {
                prompt.push_str(&format!("Failure details:\n{}\n", details));
            }
        }
        IterationOutcome::AgentError => {
            prompt.push_str(&format!(
                "Previous iteration {} encountered an agent error.\n",
                ctx.iterations_completed
            ));
            if let Some(ref details) = ctx.failure_details {
                prompt.push_str(&format!("Error: {}\n", details));
            }
        }
    }

    // Completed items
    if !ctx.completed_items.is_empty() {
        prompt.push_str(&format!(
            "\nCompleted items ({}):\n",
            ctx.completed_items.len()
        ));
        for item in &ctx.completed_items {
            prompt.push_str(&format!("  - {}\n", item));
        }
    }

    // Hook results from last iteration
    if !ctx.hook_results.is_empty() {
        prompt.push_str("\nHook results from previous iteration:\n");
        for h in &ctx.hook_results {
            let status = if h.success { "ok" } else { "FAILED" };
            prompt.push_str(&format!("  - {} [{}]: {}\n", h.name, status, h.summary));
        }
    }

    // Last commit
    if let Some(ref commit) = ctx.last_commit {
        prompt.push_str(&format!("\nLast commit: {}\n", commit));
    }

    prompt
}

pub async fn run_loop(config: LoopConfig, cancel: CancellationToken) -> anyhow::Result<()> {
    let plan_path_str = config.plan_path.display().to_string();

    // Read initial task description from system prompt or plan path context
    let task_description = format!(
        "Autonomous {} loop on plan: {}",
        match config.mode {
            LoopMode::Plan => "planning",
            LoopMode::Build => "build",
        },
        plan_path_str
    );

    // Initialize iteration context
    let mut iter_ctx = IterationContext {
        task_description,
        iterations_completed: 0,
        last_outcome: IterationOutcome::FirstIteration,
        completed_items: parse_completed_items(&config.plan_path),
        failure_details: None,
        hook_results: Vec::new(),
        last_commit: get_last_commit(&config.cwd),
    };

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

        // Build fresh prompt from deterministic context
        let prompt = build_iteration_prompt(&iter_ctx, config.mode, &plan_path_str);

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
            post_tools_hooks: config.post_tools_hooks.clone(),
        };

        let mut consumer = agent_loop(prompts, loop_config, cancel.clone());

        // Collect hook results during this iteration
        let mut iteration_hook_results: Vec<HookSummary> = Vec::new();
        let agent_error: Option<String> = None;

        // Consume all events, print to stderr, collect hook results
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
                    iteration_hook_results.push(HookSummary {
                        name: hook_name,
                        success,
                        summary,
                    });
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

        // Run validation commands and determine outcome
        let mut validation_failed = false;
        let mut failure_details: Option<String> = None;

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
                            validation_failed = true;
                            let mut details = format!("Command failed: {}\n", cmd);
                            if !stdout.is_empty() {
                                details.push_str(&format!("stdout:\n{}\n", stdout.trim()));
                            }
                            if !stderr.is_empty() {
                                details.push_str(&format!("stderr:\n{}\n", stderr.trim()));
                            }
                            failure_details = Some(details);
                            break;
                        }
                        eprintln!("[validate] ok");
                    }
                    Err(e) => {
                        eprintln!("[validate] Failed to run '{}': {}", cmd, e);
                        validation_failed = true;
                        failure_details = Some(format!("Failed to execute '{}': {}", cmd, e));
                        break;
                    }
                }
            }
        }

        // Update iteration context deterministically from filesystem/git state
        iter_ctx.iterations_completed = iteration;
        iter_ctx.completed_items = parse_completed_items(&config.plan_path);
        iter_ctx.last_commit = get_last_commit(&config.cwd);
        iter_ctx.hook_results = iteration_hook_results;

        if let Some(err) = agent_error {
            iter_ctx.last_outcome = IterationOutcome::AgentError;
            iter_ctx.failure_details = Some(err);
        } else if validation_failed {
            iter_ctx.last_outcome = IterationOutcome::ValidationFailed;
            iter_ctx.failure_details = failure_details;
        } else {
            iter_ctx.last_outcome = IterationOutcome::Success;
            iter_ctx.failure_details = None;
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
