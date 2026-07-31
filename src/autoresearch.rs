use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use rho_core::agent_loop::{agent_loop, AgentLoopConfig};
use rho_core::hooks::PostToolsHook;
use rho_core::provider_types::StreamFn;
use rho_core::tool::AgentTool;
use rho_core::types::*;

use crate::OutputFormat;

// === Types ===

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MetricDirection {
    Lower,
    Higher,
}

impl MetricDirection {
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "higher" | "up" | "maximize" => MetricDirection::Higher,
            _ => MetricDirection::Lower,
        }
    }

    fn label(&self) -> &'static str {
        match self {
            MetricDirection::Lower => "lower",
            MetricDirection::Higher => "higher",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExperimentEntry {
    pub iteration: usize,
    pub timestamp: String,
    pub description: String,
    pub metric_value: Option<f64>,
    pub wall_clock_ms: u64,
    pub status: String,
    pub commit: Option<String>,
}

pub struct AutoresearchConfig {
    pub benchmark_command: String,
    pub metric_name: String,
    pub direction: MetricDirection,
    pub metric_regex: Option<Regex>,
    pub checks_command: Option<String>,
    pub max_iterations: usize,
    pub sleep_between: Duration,
    pub objective: Option<String>,
    pub benchmark_timeout: Duration,
    pub model: Model,
    pub api_key: String,
    pub system_prompt: String,
    pub tools: Vec<Arc<dyn AgentTool>>,
    pub thinking: ThinkingLevel,
    pub cwd: PathBuf,
    pub stream_fn: StreamFn,
    pub post_tools_hooks: Vec<Arc<dyn PostToolsHook>>,
    pub output_format: OutputFormat,
}

// === Metric helpers ===

pub fn parse_metric(stdout: &str, regex: &Regex) -> Option<f64> {
    let caps = regex.captures(stdout)?;
    let value_str = caps.get(1).map(|m| m.as_str())?;
    // Strip commas for numbers like "1,523.4"
    let cleaned = value_str.replace(',', "");
    cleaned.parse::<f64>().ok()
}

pub fn is_improvement(new_val: f64, best_val: f64, direction: MetricDirection) -> bool {
    match direction {
        MetricDirection::Lower => new_val < best_val,
        MetricDirection::Higher => new_val > best_val,
    }
}

pub fn best_metric(
    entries: &[ExperimentEntry],
    direction: MetricDirection,
) -> Option<(f64, usize)> {
    entries
        .iter()
        .filter_map(|e| e.metric_value.map(|v| (v, e.iteration)))
        .reduce(|best, current| {
            if is_improvement(current.0, best.0, direction) {
                current
            } else {
                best
            }
        })
}

// === JSONL I/O ===

const JSONL_FILE: &str = "autoresearch.jsonl";
const SESSION_MD: &str = "autoresearch.md";

pub fn read_experiment_log(cwd: &Path) -> Vec<ExperimentEntry> {
    let path = cwd.join(JSONL_FILE);
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

pub fn append_experiment_entry(cwd: &Path, entry: &ExperimentEntry) -> Result<()> {
    let path = cwd.join(JSONL_FILE);
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("Failed to open {}", path.display()))?;
    let line = serde_json::to_string(entry)?;
    writeln!(file, "{}", line)?;
    Ok(())
}

// === Session markdown ===

pub fn generate_session_md(config: &AutoresearchConfig, entries: &[ExperimentEntry]) -> String {
    let mut md = String::new();

    md.push_str("# Autoresearch Session\n\n");

    // Config summary
    md.push_str("## Configuration\n\n");
    md.push_str(&format!(
        "- **Metric**: {} ({} is better)\n",
        config.metric_name,
        config.direction.label()
    ));
    md.push_str(&format!(
        "- **Benchmark**: `{}`\n",
        config.benchmark_command
    ));
    if let Some(ref checks) = config.checks_command {
        md.push_str(&format!("- **Checks**: `{}`\n", checks));
    }
    if let Some(ref obj) = config.objective {
        md.push_str(&format!("- **Objective**: {}\n", obj));
    }
    md.push('\n');

    // Best result
    if let Some((best_val, best_iter)) = best_metric(entries, config.direction) {
        let baseline = entries.first().and_then(|e| e.metric_value);
        md.push_str("## Best Result\n\n");
        md.push_str(&format!(
            "- **Best**: {:.4} {} (iteration {})\n",
            best_val, config.metric_name, best_iter
        ));
        if let Some(base) = baseline {
            if base != 0.0 {
                let pct = ((best_val - base) / base.abs()) * 100.0;
                md.push_str(&format!(
                    "- **Baseline**: {:.4} {}\n",
                    base, config.metric_name
                ));
                md.push_str(&format!("- **Improvement**: {:.1}%\n", pct.abs()));
            }
        }
        md.push('\n');
    }

    // Experiment history table
    if !entries.is_empty() {
        md.push_str("## Experiment History\n\n");
        md.push_str("| # | Description | Value | Status | Commit |\n");
        md.push_str("|---|-------------|-------|--------|--------|\n");
        for e in entries {
            let val = e
                .metric_value
                .map(|v| format!("{:.4}", v))
                .unwrap_or_else(|| "-".into());
            let commit = e.commit.as_deref().unwrap_or("-");
            md.push_str(&format!(
                "| {} | {} | {} | {} | {} |\n",
                e.iteration,
                e.description.replace('|', "\\|"),
                val,
                e.status,
                commit
            ));
        }
        md.push('\n');
    }

    // Sections for the agent to maintain
    md.push_str("## Strategies Tried\n\n<!-- Agent: update this section with strategies you've attempted -->\n\n");
    md.push_str("## Dead Ends\n\n<!-- Agent: note approaches that didn't work and why -->\n\n");
    md.push_str(
        "## Notes\n\n<!-- Agent: any observations about the codebase or metric behavior -->\n\n",
    );

    md
}

fn write_session_md(cwd: &Path, content: &str) -> Result<()> {
    let path = cwd.join(SESSION_MD);
    // If the file already exists, preserve agent-maintained sections
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let merged = merge_session_md(content, &existing);
        std::fs::write(&path, merged)?;
    } else {
        std::fs::write(&path, content)?;
    }
    Ok(())
}

/// Merge generated header/table sections with agent-maintained sections from existing file.
fn merge_session_md(generated: &str, existing: &str) -> String {
    // Extract agent-maintained sections from existing
    let sections = ["## Strategies Tried", "## Dead Ends", "## Notes"];
    let mut result = String::new();

    // Take everything from generated up to first agent section
    let gen_cutoff = sections
        .iter()
        .filter_map(|s| generated.find(s))
        .min()
        .unwrap_or(generated.len());
    result.push_str(&generated[..gen_cutoff]);

    // For each section, take from existing if present, else from generated
    for section in &sections {
        let existing_content = extract_section(existing, section);
        let generated_content = extract_section(generated, section);
        let content = if existing_content.trim().len() > generated_content.trim().len() {
            existing_content
        } else {
            generated_content
        };
        result.push_str(&content);
    }

    result
}

fn extract_section(md: &str, heading: &str) -> String {
    let start = match md.find(heading) {
        Some(pos) => pos,
        None => return format!("{}\n\n", heading),
    };
    // Find next ## heading or end
    let after_heading = start + heading.len();
    let end = md[after_heading..]
        .find("\n## ")
        .map(|p| after_heading + p + 1)
        .unwrap_or(md.len());
    md[start..end].to_string()
}

// === Git helpers ===

fn git_has_changes(cwd: &Path) -> bool {
    std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(cwd)
        .output()
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false)
}

fn git_commit_changes(cwd: &Path, message: &str) -> Result<Option<String>> {
    // Stage all changes
    let add = std::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(cwd)
        .output()
        .context("git add failed")?;
    if !add.status.success() {
        anyhow::bail!("git add failed: {}", String::from_utf8_lossy(&add.stderr));
    }

    let commit = std::process::Command::new("git")
        .args(["commit", "-m", message])
        .current_dir(cwd)
        .output()
        .context("git commit failed")?;
    if !commit.status.success() {
        let stderr = String::from_utf8_lossy(&commit.stderr);
        if stderr.contains("nothing to commit") {
            return Ok(None);
        }
        anyhow::bail!("git commit failed: {}", stderr);
    }

    // Get short hash
    let hash = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(cwd)
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
            } else {
                None
            }
        });
    Ok(hash)
}

fn git_revert_changes(cwd: &Path) -> Result<()> {
    // Reset staged changes
    let _ = std::process::Command::new("git")
        .args(["reset", "HEAD", "--"])
        .current_dir(cwd)
        .output();
    // Discard working tree changes
    let checkout = std::process::Command::new("git")
        .args(["checkout", "--", "."])
        .current_dir(cwd)
        .output()
        .context("git checkout failed")?;
    if !checkout.status.success() {
        anyhow::bail!(
            "git checkout failed: {}",
            String::from_utf8_lossy(&checkout.stderr)
        );
    }
    // Clean untracked files
    let _ = std::process::Command::new("git")
        .args(["clean", "-fd"])
        .current_dir(cwd)
        .output();
    Ok(())
}

// === Benchmark runner ===

async fn run_benchmark(
    command: &str,
    timeout: Duration,
    cwd: &Path,
    metric_regex: Option<&Regex>,
) -> Result<(Option<f64>, u64)> {
    let start = Instant::now();

    let result = tokio::time::timeout(
        timeout,
        tokio::process::Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(cwd)
            .output(),
    )
    .await;

    let wall_clock_ms = start.elapsed().as_millis() as u64;

    match result {
        Ok(Ok(output)) => {
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                anyhow::bail!(
                    "Benchmark failed (exit {}): {}",
                    output.status,
                    stderr.chars().take(500).collect::<String>()
                );
            }
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr_str = String::from_utf8_lossy(&output.stderr);
            let combined = format!("{}\n{}", stdout, stderr_str);

            let metric_value = metric_regex.and_then(|re| parse_metric(&combined, re));
            Ok((metric_value, wall_clock_ms))
        }
        Ok(Err(e)) => anyhow::bail!("Failed to execute benchmark: {}", e),
        Err(_) => anyhow::bail!("Benchmark timed out after {}s", timeout.as_secs()),
    }
}

async fn run_checks(command: &str, cwd: &Path) -> Result<bool> {
    let output = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(cwd)
        .output()
        .await
        .context("Failed to execute checks command")?;
    Ok(output.status.success())
}

// === Prompt builder ===

fn build_autoresearch_prompt(
    config: &AutoresearchConfig,
    entries: &[ExperimentEntry],
    session_md: &str,
    iteration: usize,
) -> String {
    let mut prompt = String::new();

    prompt.push_str(&format!(
        "You are optimizing **{}** ({} is better).\n",
        config.metric_name,
        config.direction.label()
    ));
    prompt.push_str(&format!(
        "Benchmark command: `{}`\n",
        config.benchmark_command
    ));
    if let Some(ref checks) = config.checks_command {
        prompt.push_str(&format!("Checks command: `{}`\n", checks));
    }
    prompt.push('\n');

    if let Some((best_val, best_iter)) = best_metric(entries, config.direction) {
        let baseline = entries.first().and_then(|e| e.metric_value);
        prompt.push_str(&format!(
            "Current best: {:.4} {} (iteration {})\n",
            best_val, config.metric_name, best_iter
        ));
        if let Some(base) = baseline {
            prompt.push_str(&format!("Baseline: {:.4} {}\n", base, config.metric_name));
            if base != 0.0 {
                let pct = ((best_val - base) / base.abs()) * 100.0;
                prompt.push_str(&format!("Improvement so far: {:.1}%\n", pct.abs()));
            }
        }
        prompt.push('\n');
    }

    // Session context (the full MD file)
    prompt.push_str("## Session Context\n\n");
    prompt.push_str(session_md);
    prompt.push('\n');

    // Recent experiments (last 10)
    let recent: Vec<_> = entries.iter().rev().take(10).rev().collect();
    if !recent.is_empty() {
        prompt.push_str("## Recent Experiments (last 10)\n\n");
        prompt.push_str("| # | Description | Value | Status |\n");
        prompt.push_str("|---|-------------|-------|--------|\n");
        for e in &recent {
            let val = e
                .metric_value
                .map(|v| format!("{:.4}", v))
                .unwrap_or_else(|| "-".into());
            prompt.push_str(&format!(
                "| {} | {} | {} | {} |\n",
                e.iteration,
                e.description.replace('|', "\\|"),
                val,
                e.status
            ));
        }
        prompt.push('\n');
    }

    prompt.push_str(&format!("## Your Task (Iteration {})\n\n", iteration));
    prompt.push_str("1. Analyze experiment history and identify promising strategies\n");
    prompt.push_str(&format!(
        "2. Generate a hypothesis for what might improve {}\n",
        config.metric_name
    ));
    prompt.push_str("3. Implement ONE focused change using the available tools\n");
    prompt
        .push_str("4. Update \"Strategies Tried\" or \"Dead Ends\" sections in autoresearch.md\n");
    prompt.push_str("5. Do NOT run the benchmark -- the system runs it after you finish\n");
    prompt.push_str("6. Do NOT run git commit/add/push -- the system handles git\n");
    prompt
        .push_str("7. If all viable optimizations are exhausted, write \"EXHAUSTED\" to .stop\n\n");
    prompt.push_str("Explain what you changed and why in your final message.\n");

    prompt
}

// === Main loop ===

pub async fn run_autoresearch(config: AutoresearchConfig, cancel: CancellationToken) -> Result<()> {
    let json = config.output_format == OutputFormat::StreamJson;

    macro_rules! emit_json {
        ($($json:tt)+) => {
            if json {
                println!("{}", serde_json::json!($($json)+));
            }
        };
    }

    eprintln!(
        "[autoresearch] Starting: {} ({} is better)",
        config.metric_name,
        config.direction.label()
    );
    emit_json!({
        "type": "autoresearch_start",
        "metric": config.metric_name,
        "direction": config.direction.label(),
        "benchmark": config.benchmark_command
    });

    // Read existing log (for resume)
    let mut entries = read_experiment_log(&config.cwd);
    let start_iteration = if entries.is_empty() { 0 } else { entries.len() };

    // Baseline
    if entries.is_empty() {
        eprintln!("[autoresearch] Running baseline benchmark...");

        // Ensure clean working tree
        if git_has_changes(&config.cwd) {
            eprintln!("[autoresearch] WARNING: dirty working tree, reverting for baseline");
            git_revert_changes(&config.cwd)?;
        }

        match run_benchmark(
            &config.benchmark_command,
            config.benchmark_timeout,
            &config.cwd,
            config.metric_regex.as_ref(),
        )
        .await
        {
            Ok((metric_value, wall_clock_ms)) => {
                let effective_metric = metric_value.or(Some(wall_clock_ms as f64));
                eprintln!(
                    "[autoresearch] Baseline: {:?} {} ({}ms)",
                    effective_metric, config.metric_name, wall_clock_ms
                );
                emit_json!({
                    "type": "baseline",
                    "metric_value": effective_metric,
                    "wall_clock_ms": wall_clock_ms
                });

                let entry = ExperimentEntry {
                    iteration: 0,
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    description: "Baseline measurement".into(),
                    metric_value: effective_metric,
                    wall_clock_ms,
                    status: "baseline".into(),
                    commit: None,
                };
                append_experiment_entry(&config.cwd, &entry)?;
                entries.push(entry);

                // Write initial session MD
                let md = generate_session_md(&config, &entries);
                write_session_md(&config.cwd, &md)?;
            }
            Err(e) => {
                anyhow::bail!(
                    "Baseline benchmark failed: {}. Fix the benchmark command and retry.",
                    e
                );
            }
        }
    } else {
        eprintln!(
            "[autoresearch] Resuming from {} existing entries",
            entries.len()
        );
    }

    // Main iteration loop
    let iteration_start = start_iteration + 1;
    let iteration_end = start_iteration + config.max_iterations;

    for iteration in iteration_start..=iteration_end {
        if cancel.is_cancelled() {
            eprintln!("[autoresearch] Cancelled");
            emit_json!({"type": "autoresearch_cancelled"});
            break;
        }

        // Check .stop file
        let stop_file = config.cwd.join(".stop");
        if stop_file.exists() {
            let reason = std::fs::read_to_string(&stop_file).unwrap_or_default();
            let _ = std::fs::remove_file(&stop_file);
            eprintln!("[autoresearch] Stopped: {}", reason.trim());
            emit_json!({"type": "autoresearch_stopped", "reason": reason.trim()});
            break;
        }

        eprintln!(
            "\n[autoresearch] === Iteration {}/{} ===",
            iteration - start_iteration,
            config.max_iterations
        );
        emit_json!({
            "type": "experiment_start",
            "iteration": iteration
        });

        // Ensure clean working tree
        if git_has_changes(&config.cwd) {
            eprintln!("[autoresearch] Cleaning dirty working tree from previous iteration");
            git_revert_changes(&config.cwd)?;
        }

        // Re-read state from disk (deterministic context)
        entries = read_experiment_log(&config.cwd);
        let session_md = std::fs::read_to_string(config.cwd.join(SESSION_MD)).unwrap_or_default();

        // Build prompt and run agent
        let prompt = build_autoresearch_prompt(&config, &entries, &session_md, iteration);

        let messages = vec![Message::User {
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

        let mut consumer = agent_loop(messages, loop_config, cancel.clone());
        let mut agent_description = String::new();

        // Consume agent events
        while let Some(event) = consumer.next().await {
            if json {
                match event {
                    AgentEvent::MessageUpdate {
                        event: AssistantStreamEvent::TextDelta { delta, .. },
                        ..
                    } => {
                        agent_description.push_str(&delta);
                        println!(
                            "{}",
                            serde_json::json!({
                                "type": "text_delta",
                                "text": delta
                            })
                        );
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
                    AgentEvent::AgentEnd { .. } => {}
                    _ => {}
                }
            } else {
                match event {
                    AgentEvent::MessageUpdate {
                        event: AssistantStreamEvent::TextDelta { delta, .. },
                        ..
                    } => {
                        agent_description.push_str(&delta);
                        eprint!("{}", delta);
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
        }

        // Check .stop file after agent
        let stop_file = config.cwd.join(".stop");
        if stop_file.exists() {
            let reason = std::fs::read_to_string(&stop_file).unwrap_or_default();
            let _ = std::fs::remove_file(&stop_file);
            eprintln!("[autoresearch] Agent stopped: {}", reason.trim());
            emit_json!({"type": "autoresearch_stopped", "reason": reason.trim()});
            break;
        }

        // Truncate description for logging
        let description = agent_description
            .lines()
            .last()
            .unwrap_or("no description")
            .chars()
            .take(200)
            .collect::<String>();

        // Evaluate: check for changes, run checks, run benchmark
        if !git_has_changes(&config.cwd) {
            eprintln!("[autoresearch] No changes made, skipping benchmark");
            let entry = ExperimentEntry {
                iteration,
                timestamp: chrono::Utc::now().to_rfc3339(),
                description: description.clone(),
                metric_value: None,
                wall_clock_ms: 0,
                status: "no_change".into(),
                commit: None,
            };
            append_experiment_entry(&config.cwd, &entry)?;
            entries.push(entry);
            continue;
        }

        // Run checks if configured
        if let Some(ref checks_cmd) = config.checks_command {
            eprintln!("[autoresearch] Running checks: {}", checks_cmd);
            emit_json!({"type": "checks_start"});

            match run_checks(checks_cmd, &config.cwd).await {
                Ok(true) => {
                    eprintln!("[autoresearch] Checks passed");
                    emit_json!({"type": "checks_result", "success": true});
                }
                Ok(false) => {
                    eprintln!("[autoresearch] Checks failed, reverting");
                    emit_json!({"type": "checks_result", "success": false});
                    git_revert_changes(&config.cwd)?;
                    let entry = ExperimentEntry {
                        iteration,
                        timestamp: chrono::Utc::now().to_rfc3339(),
                        description,
                        metric_value: None,
                        wall_clock_ms: 0,
                        status: "checks_failed".into(),
                        commit: None,
                    };
                    append_experiment_entry(&config.cwd, &entry)?;
                    entries.push(entry);

                    let md = generate_session_md(&config, &entries);
                    write_session_md(&config.cwd, &md)?;
                    sleep_between_iterations(&config, iteration, iteration_end, &cancel).await;
                    continue;
                }
                Err(e) => {
                    eprintln!("[autoresearch] Checks error: {}, reverting", e);
                    git_revert_changes(&config.cwd)?;
                    let entry = ExperimentEntry {
                        iteration,
                        timestamp: chrono::Utc::now().to_rfc3339(),
                        description,
                        metric_value: None,
                        wall_clock_ms: 0,
                        status: "checks_failed".into(),
                        commit: None,
                    };
                    append_experiment_entry(&config.cwd, &entry)?;
                    entries.push(entry);

                    let md = generate_session_md(&config, &entries);
                    write_session_md(&config.cwd, &md)?;
                    sleep_between_iterations(&config, iteration, iteration_end, &cancel).await;
                    continue;
                }
            }
        }

        // Run benchmark
        eprintln!(
            "[autoresearch] Running benchmark: {}",
            config.benchmark_command
        );
        emit_json!({"type": "benchmark_start"});

        match run_benchmark(
            &config.benchmark_command,
            config.benchmark_timeout,
            &config.cwd,
            config.metric_regex.as_ref(),
        )
        .await
        {
            Ok((metric_value, wall_clock_ms)) => {
                // If no regex match, fall back to wall-clock
                let effective_metric = metric_value.or_else(|| {
                    if config.metric_regex.is_some() {
                        eprintln!("[autoresearch] WARNING: metric regex didn't match, using wall-clock time");
                    }
                    Some(wall_clock_ms as f64)
                });

                eprintln!(
                    "[autoresearch] Result: {:?} {} ({}ms)",
                    effective_metric, config.metric_name, wall_clock_ms
                );
                emit_json!({
                    "type": "benchmark_result",
                    "metric_value": effective_metric,
                    "wall_clock_ms": wall_clock_ms
                });

                // Compare to best
                let current_best = best_metric(&entries, config.direction);
                let is_better = match (effective_metric, current_best) {
                    (Some(new_val), Some((best_val, _))) => {
                        is_improvement(new_val, best_val, config.direction)
                    }
                    (Some(_), None) => true,
                    _ => false,
                };

                if is_better {
                    let commit_msg = format!(
                        "[autoresearch] iter {}: {} = {:.4} ({})",
                        iteration,
                        config.metric_name,
                        effective_metric.unwrap_or(0.0),
                        description.chars().take(80).collect::<String>()
                    );
                    let commit_hash = git_commit_changes(&config.cwd, &commit_msg)?;
                    eprintln!("[autoresearch] IMPROVED! Committed as {:?}", commit_hash);

                    let entry = ExperimentEntry {
                        iteration,
                        timestamp: chrono::Utc::now().to_rfc3339(),
                        description,
                        metric_value: effective_metric,
                        wall_clock_ms,
                        status: "improved".into(),
                        commit: commit_hash.clone(),
                    };
                    emit_json!({
                        "type": "experiment_result",
                        "iteration": iteration,
                        "status": "improved",
                        "metric_value": effective_metric,
                        "best_value": effective_metric,
                        "commit": commit_hash
                    });
                    append_experiment_entry(&config.cwd, &entry)?;
                    entries.push(entry);
                } else {
                    eprintln!("[autoresearch] Regressed, reverting");
                    git_revert_changes(&config.cwd)?;

                    let entry = ExperimentEntry {
                        iteration,
                        timestamp: chrono::Utc::now().to_rfc3339(),
                        description,
                        metric_value: effective_metric,
                        wall_clock_ms,
                        status: "regressed".into(),
                        commit: None,
                    };
                    emit_json!({
                        "type": "experiment_result",
                        "iteration": iteration,
                        "status": "regressed",
                        "metric_value": effective_metric,
                        "best_value": current_best.map(|(v, _)| v)
                    });
                    append_experiment_entry(&config.cwd, &entry)?;
                    entries.push(entry);
                }
            }
            Err(e) => {
                eprintln!("[autoresearch] Benchmark failed: {}, reverting", e);
                git_revert_changes(&config.cwd)?;

                let entry = ExperimentEntry {
                    iteration,
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    description,
                    metric_value: None,
                    wall_clock_ms: 0,
                    status: "benchmark_failed".into(),
                    commit: None,
                };
                emit_json!({
                    "type": "experiment_result",
                    "iteration": iteration,
                    "status": "benchmark_failed"
                });
                append_experiment_entry(&config.cwd, &entry)?;
                entries.push(entry);
            }
        }

        // Regenerate session MD
        let md = generate_session_md(&config, &entries);
        write_session_md(&config.cwd, &md)?;

        // Sleep between iterations
        sleep_between_iterations(&config, iteration, iteration_end, &cancel).await;
    }

    // Final summary
    let baseline = entries.first().and_then(|e| e.metric_value);
    let best = best_metric(&entries, config.direction);
    let total_iterations = entries.len().saturating_sub(1); // exclude baseline

    if let (Some(base), Some((best_val, _))) = (baseline, best) {
        let improvement_pct = if base != 0.0 {
            ((best_val - base) / base.abs()) * 100.0
        } else {
            0.0
        };
        eprintln!(
            "\n[autoresearch] Done. Best: {:.4} (baseline: {:.4}, improvement: {:.1}%, {} iterations)",
            best_val, base, improvement_pct.abs(), total_iterations
        );
        emit_json!({
            "type": "autoresearch_done",
            "best_value": best_val,
            "baseline": base,
            "improvement_pct": improvement_pct.abs(),
            "total_iterations": total_iterations
        });
    } else {
        eprintln!(
            "\n[autoresearch] Done. {} iterations completed.",
            total_iterations
        );
        emit_json!({
            "type": "autoresearch_done",
            "total_iterations": total_iterations
        });
    }

    Ok(())
}

async fn sleep_between_iterations(
    config: &AutoresearchConfig,
    iteration: usize,
    max: usize,
    cancel: &CancellationToken,
) {
    if iteration < max {
        let sleep_secs = config.sleep_between.as_secs();
        if sleep_secs > 0 {
            eprintln!("[autoresearch] Sleeping {}s...", sleep_secs);
            tokio::select! {
                _ = tokio::time::sleep(config.sleep_between) => {},
                _ = cancel.cancelled() => {},
            }
        }
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

// === Tests ===

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_metric_basic() {
        let re = Regex::new(r"score:\s*(\d+\.?\d*)").unwrap();
        assert_eq!(parse_metric("score: 42.5", &re), Some(42.5));
        assert_eq!(parse_metric("score: 100", &re), Some(100.0));
        assert_eq!(parse_metric("no match here", &re), None);
    }

    #[test]
    fn test_parse_metric_with_commas() {
        let re = Regex::new(r"time:\s*([\d,]+\.?\d*)").unwrap();
        assert_eq!(parse_metric("time: 1,523.4", &re), Some(1523.4));
    }

    #[test]
    fn test_parse_metric_multiline() {
        let re = Regex::new(r"latency_ns\s+(\d+)").unwrap();
        let output = "running benchmark...\nlatency_ns 1523\ntest passed\n";
        assert_eq!(parse_metric(output, &re), Some(1523.0));
    }

    #[test]
    fn test_is_improvement_lower() {
        assert!(is_improvement(10.0, 20.0, MetricDirection::Lower));
        assert!(!is_improvement(20.0, 10.0, MetricDirection::Lower));
        assert!(!is_improvement(10.0, 10.0, MetricDirection::Lower));
    }

    #[test]
    fn test_is_improvement_higher() {
        assert!(is_improvement(20.0, 10.0, MetricDirection::Higher));
        assert!(!is_improvement(10.0, 20.0, MetricDirection::Higher));
        assert!(!is_improvement(10.0, 10.0, MetricDirection::Higher));
    }

    #[test]
    fn test_best_metric_lower() {
        let entries = vec![
            ExperimentEntry {
                iteration: 0,
                timestamp: String::new(),
                description: "baseline".into(),
                metric_value: Some(100.0),
                wall_clock_ms: 0,
                status: "baseline".into(),
                commit: None,
            },
            ExperimentEntry {
                iteration: 1,
                timestamp: String::new(),
                description: "try 1".into(),
                metric_value: Some(80.0),
                wall_clock_ms: 0,
                status: "improved".into(),
                commit: Some("abc".into()),
            },
            ExperimentEntry {
                iteration: 2,
                timestamp: String::new(),
                description: "try 2".into(),
                metric_value: Some(90.0),
                wall_clock_ms: 0,
                status: "regressed".into(),
                commit: None,
            },
        ];
        let (val, iter) = best_metric(&entries, MetricDirection::Lower).unwrap();
        assert_eq!(val, 80.0);
        assert_eq!(iter, 1);
    }

    #[test]
    fn test_best_metric_higher() {
        let entries = vec![
            ExperimentEntry {
                iteration: 0,
                timestamp: String::new(),
                description: "baseline".into(),
                metric_value: Some(50.0),
                wall_clock_ms: 0,
                status: "baseline".into(),
                commit: None,
            },
            ExperimentEntry {
                iteration: 1,
                timestamp: String::new(),
                description: "try 1".into(),
                metric_value: Some(75.0),
                wall_clock_ms: 0,
                status: "improved".into(),
                commit: Some("def".into()),
            },
        ];
        let (val, iter) = best_metric(&entries, MetricDirection::Higher).unwrap();
        assert_eq!(val, 75.0);
        assert_eq!(iter, 1);
    }

    #[test]
    fn test_best_metric_empty() {
        assert!(best_metric(&[], MetricDirection::Lower).is_none());
    }

    #[test]
    fn test_read_write_jsonl() {
        let dir = tempfile::tempdir().unwrap();
        let entry = ExperimentEntry {
            iteration: 0,
            timestamp: "2026-01-01T00:00:00Z".into(),
            description: "test entry".into(),
            metric_value: Some(42.0),
            wall_clock_ms: 1000,
            status: "baseline".into(),
            commit: None,
        };
        append_experiment_entry(dir.path(), &entry).unwrap();
        append_experiment_entry(
            dir.path(),
            &ExperimentEntry {
                iteration: 1,
                timestamp: "2026-01-01T00:01:00Z".into(),
                description: "second".into(),
                metric_value: Some(40.0),
                wall_clock_ms: 900,
                status: "improved".into(),
                commit: Some("abc1234".into()),
            },
        )
        .unwrap();

        let entries = read_experiment_log(dir.path());
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].iteration, 0);
        assert_eq!(entries[0].metric_value, Some(42.0));
        assert_eq!(entries[1].iteration, 1);
        assert_eq!(entries[1].commit, Some("abc1234".into()));
    }

    #[test]
    fn test_read_empty_jsonl() {
        let dir = tempfile::tempdir().unwrap();
        let entries = read_experiment_log(dir.path());
        assert!(entries.is_empty());
    }

    #[test]
    fn test_generate_session_md() {
        // Just verify it doesn't panic and contains expected sections
        let config = AutoresearchConfig {
            benchmark_command: "echo 42".into(),
            metric_name: "score".into(),
            direction: MetricDirection::Higher,
            metric_regex: None,
            checks_command: None,
            max_iterations: 10,
            sleep_between: Duration::from_secs(5),
            objective: Some("maximize score".into()),
            benchmark_timeout: Duration::from_secs(60),
            model: Model {
                id: "test".into(),
                name: "test".into(),
                provider: "test".into(),
                base_url: String::new(),
                reasoning: false,
                context_window: 100000,
                max_tokens: 4096,
            },
            api_key: String::new(),
            system_prompt: String::new(),
            tools: vec![],
            thinking: ThinkingLevel::Off,
            cwd: PathBuf::from("/tmp"),
            stream_fn: Arc::new(|_, _, _| panic!("not used in test")),
            post_tools_hooks: vec![],
            output_format: OutputFormat::Text,
        };

        let entries = vec![ExperimentEntry {
            iteration: 0,
            timestamp: "2026-01-01T00:00:00Z".into(),
            description: "Baseline measurement".into(),
            metric_value: Some(50.0),
            wall_clock_ms: 1000,
            status: "baseline".into(),
            commit: None,
        }];

        let md = generate_session_md(&config, &entries);
        assert!(md.contains("# Autoresearch Session"));
        assert!(md.contains("score"));
        assert!(md.contains("higher is better"));
        assert!(md.contains("maximize score"));
        assert!(md.contains("Baseline measurement"));
        assert!(md.contains("## Strategies Tried"));
        assert!(md.contains("## Dead Ends"));
    }

    #[test]
    fn test_metric_direction_from_str() {
        assert_eq!(MetricDirection::from_str("lower"), MetricDirection::Lower);
        assert_eq!(MetricDirection::from_str("higher"), MetricDirection::Higher);
        assert_eq!(MetricDirection::from_str("up"), MetricDirection::Higher);
        assert_eq!(
            MetricDirection::from_str("maximize"),
            MetricDirection::Higher
        );
        assert_eq!(
            MetricDirection::from_str("anything"),
            MetricDirection::Lower
        );
    }

    #[test]
    fn test_extract_section() {
        let md = "## Config\nstuff\n## Strategies Tried\nsome strategy\n## Dead Ends\nnothing\n## Notes\nsome note\n";
        let section = extract_section(md, "## Strategies Tried");
        assert!(section.contains("some strategy"));
        assert!(!section.contains("nothing")); // shouldn't include Dead Ends
    }
}
