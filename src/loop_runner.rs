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
    pub output_format: crate::OutputFormat,
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

    let json = config.output_format == crate::OutputFormat::StreamJson;

    // Helper macro for JSON output on stdout
    macro_rules! emit_json {
        ($($json:tt)+) => {
            if json {
                println!("{}", serde_json::json!($($json)+));
            }
        };
    }

    for iteration in 1..=config.max_iterations {
        if cancel.is_cancelled() {
            eprintln!("[loop] Cancelled");
            emit_json!({"type": "loop_cancelled"});
            break;
        }

        // Check .stop file
        let stop_file = config.cwd.join(".stop");
        if stop_file.exists() {
            let reason = std::fs::read_to_string(&stop_file).unwrap_or_default();
            let _ = std::fs::remove_file(&stop_file);
            eprintln!("[loop] Stopped: {}", reason.trim());
            emit_json!({"type": "loop_stopped", "reason": reason.trim()});
            break;
        }

        let mode_str = format!("{:?}", config.mode).to_lowercase();
        eprintln!(
            "\n[loop] === Iteration {}/{} ({} mode) ===",
            iteration, config.max_iterations, mode_str
        );
        emit_json!({
            "type": "iteration_start",
            "iteration": iteration,
            "max_iterations": config.max_iterations,
            "mode": mode_str
        });

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
            pre_tool_hooks: vec![],
            lifecycle_hooks: vec![],
            shared_messages: None,
        };

        let mut consumer = agent_loop(prompts, loop_config, cancel.clone());

        // Collect hook results during this iteration
        let mut iteration_hook_results: Vec<HookSummary> = Vec::new();
        let agent_error: Option<String> = None;

        // Consume all events with format-switched output
        while let Some(event) = consumer.next().await {
            if json {
                match event {
                    AgentEvent::MessageUpdate { event, .. } => {
                        if let AssistantStreamEvent::TextDelta { delta, .. } = event {
                            println!(
                                "{}",
                                serde_json::json!({
                                    "type": "text_delta",
                                    "text": delta
                                })
                            );
                        }
                    }
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
                        println!(
                            "{}",
                            serde_json::json!({
                                "type": "tool_start",
                                "tool_name": tool_name,
                                "tool_id": tool_call_id,
                                "input_summary": input_summary
                            })
                        );
                    }
                    AgentEvent::ToolExecutionEnd {
                        tool_call_id,
                        tool_name,
                        is_error,
                        ..
                    } => {
                        println!(
                            "{}",
                            serde_json::json!({
                                "type": "tool_result",
                                "tool_name": tool_name,
                                "tool_id": tool_call_id,
                                "success": !is_error
                            })
                        );
                    }
                    AgentEvent::PostToolsHookStart { hook_name } => {
                        println!(
                            "{}",
                            serde_json::json!({
                                "type": "hook_start",
                                "hook_name": hook_name
                            })
                        );
                    }
                    AgentEvent::PostToolsHookEnd {
                        hook_name,
                        success,
                        summary,
                    } => {
                        println!(
                            "{}",
                            serde_json::json!({
                                "type": "hook_result",
                                "hook_name": hook_name,
                                "success": success,
                                "summary": summary
                            })
                        );
                        iteration_hook_results.push(HookSummary {
                            name: hook_name,
                            success,
                            summary,
                        });
                    }
                    AgentEvent::ContextCompacted {
                        original_estimate,
                        compacted_estimate,
                        ..
                    } => {
                        println!(
                            "{}",
                            serde_json::json!({
                                "type": "context_compacted",
                                "original_estimate": original_estimate,
                                "compacted_estimate": compacted_estimate
                            })
                        );
                    }
                    AgentEvent::AgentEnd { .. } => {}
                    _ => {}
                }
            } else {
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
        }

        // Check .stop file again after agent exits
        let stop_file = config.cwd.join(".stop");
        if stop_file.exists() {
            let reason = std::fs::read_to_string(&stop_file).unwrap_or_default();
            let _ = std::fs::remove_file(&stop_file);
            eprintln!("[loop] Stopped after iteration: {}", reason.trim());
            emit_json!({"type": "loop_stopped", "reason": reason.trim()});
            break;
        }

        // Run validation commands and determine outcome
        let mut validation_failed = false;
        let mut failure_details: Option<String> = None;

        if !config.validation_commands.is_empty() {
            eprintln!("[loop] Running validation...");
            for cmd in &config.validation_commands {
                eprintln!("[validate] $ {}", cmd);
                emit_json!({"type": "validate_start", "command": cmd.clone()});

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
                            emit_json!({
                                "type": "validate_result",
                                "command": cmd.clone(),
                                "success": false
                            });
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
                        emit_json!({
                            "type": "validate_result",
                            "command": cmd.clone(),
                            "success": true
                        });
                    }
                    Err(e) => {
                        eprintln!("[validate] Failed to run '{}': {}", cmd, e);
                        emit_json!({
                            "type": "validate_result",
                            "command": cmd.clone(),
                            "success": false
                        });
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

        let outcome = if let Some(err) = agent_error {
            iter_ctx.last_outcome = IterationOutcome::AgentError;
            iter_ctx.failure_details = Some(err);
            "error"
        } else if validation_failed {
            iter_ctx.last_outcome = IterationOutcome::ValidationFailed;
            iter_ctx.failure_details = failure_details;
            "validation_failed"
        } else {
            iter_ctx.last_outcome = IterationOutcome::Success;
            iter_ctx.failure_details = None;
            "success"
        };

        emit_json!({
            "type": "iteration_end",
            "iteration": iteration,
            "outcome": outcome
        });

        // Sleep between iterations
        if iteration < config.max_iterations {
            let sleep_secs = config.sleep_between.as_secs();
            eprintln!("[loop] Sleeping {}s before next iteration...", sleep_secs);
            emit_json!({"type": "loop_sleep", "seconds": sleep_secs});
            tokio::select! {
                _ = tokio::time::sleep(config.sleep_between) => {},
                _ = cancel.cancelled() => {
                    eprintln!("[loop] Cancelled during sleep");
                    emit_json!({"type": "loop_cancelled"});
                    break;
                }
            }
        }
    }

    eprintln!("[loop] Done");
    emit_json!({"type": "loop_done"});
    Ok(())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
