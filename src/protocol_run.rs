use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rho_core::agent_loop::{agent_loop, AgentLoopConfig};
use rho_core::config::ProjectConfig;
use rho_core::models::{ModelConfig, ModelRegistry, ProviderType};
use rho_core::types as core;
use rho_protocol as protocol;
use serde::Serialize;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::build_tools;

const PROTOCOL_SYSTEM_PROMPT: &str =
    "You are a coding assistant. Use only the tools supplied with this execution.";

pub async fn run(request_file: &Path, events: &str) -> i32 {
    debug_assert_eq!(events, "jsonl");
    let request = match read_request(request_file) {
        Ok(request) => request,
        Err(error) => {
            eprintln!("invalid rho.run/v1 request: {error}");
            return 2;
        }
    };

    let mut emitter = Emitter::new(request.run_id.clone());
    if let Err(error) = preflight(&request) {
        let _ = emitter.failure("grant_denied", error, false);
        return 0;
    }

    let cwd = match workspace(&request) {
        Ok(cwd) => cwd,
        Err(error) => {
            let _ = emitter.failure("invalid_request", error.to_string(), false);
            return 0;
        }
    };
    if let Err(error) = validate_supported_tools(&request) {
        let _ = emitter.failure("grant_denied", error, false);
        return 0;
    }
    // Protocol execution must not silently ingest RHO.md, CLAUDE.md, global
    // memories, skills, or commands. Those ambient files are outside the
    // request/grant contract and could be exfiltrated to the provider.
    let project_config = ProjectConfig::default();
    let model_config = model_config(&request);
    let model = match rho_core::protocol::model_from_protocol(&request.model, &model_config) {
        Ok(model) => model,
        Err(error) => {
            let _ = emitter.failure("invalid_request", error.to_string(), false);
            return 0;
        }
    };
    let api_key = match resolve_credential(&request, &model_config) {
        Ok(key) => key,
        Err(error) => {
            let _ = emitter.failure("credential_unavailable", error, false);
            return 0;
        }
    };
    let messages = match rho_core::protocol::messages_from_protocol(&request.messages, now_ms()) {
        Ok(messages) => messages,
        Err(error) => {
            let _ = emitter.failure("invalid_request", error.to_string(), false);
            return 0;
        }
    };

    let allowed = Some(request.grant.tools.tools.clone());
    let tools = build_tools(&cwd, &allowed, &project_config);
    let enabled_tools: Vec<String> = tools.iter().map(|tool| tool.name().to_owned()).collect();
    if enabled_tools.len() != request.grant.tools.tools.len() {
        let unknown: Vec<_> = request
            .grant
            .tools
            .tools
            .iter()
            .filter(|name| !enabled_tools.contains(name))
            .cloned()
            .collect();
        let _ = emitter.failure(
            "grant_denied",
            format!(
                "unknown or unavailable granted tools: {}",
                unknown.join(", ")
            ),
            false,
        );
        return 0;
    }

    let system_prompt = request
        .system_prompt
        .clone()
        .unwrap_or_else(|| PROTOCOL_SYSTEM_PROMPT.to_owned());
    let max_tokens = request
        .limits
        .max_output_tokens
        .map(|_| model_config.max_tokens);
    let config = AgentLoopConfig {
        model,
        api_key,
        system_prompt,
        tools,
        thinking: core::ThinkingLevel::Off,
        max_tokens,
        stream_fn: rho_provider::stream_fn_for_model(&model_config),
        get_steering_messages: None,
        get_follow_up_messages: None,
        transform_messages: None,
        post_tools_hooks: vec![],
        pre_tool_hooks: vec![Arc::new(PathGrantHook::new(&request, &cwd))],
        lifecycle_hooks: vec![],
        shared_messages: None,
    };

    if emitter
        .event(
            "run.started",
            &json!({
                "provider": request.model.provider,
                "model": request.model.id,
                "limits": request.limits,
                "tools": enabled_tools,
            }),
        )
        .is_err()
    {
        return 5;
    }

    let cancel = CancellationToken::new();
    install_cancellation(
        cancel.clone(),
        effective_deadline(
            request.limits.deadline.as_deref(),
            &request.grant.expires_at,
        ),
    );
    let mut stream = agent_loop(messages, config, cancel.clone());
    let mut turns = 0_u32;
    let mut final_messages = None;
    let mut failed = false;
    while let Some(event) = stream.next().await {
        match event {
            core::AgentEvent::TurnStart => {
                turns += 1;
                if request.limits.max_turns.is_some_and(|limit| turns > limit) {
                    failed = true;
                    cancel.cancel();
                }
            }
            core::AgentEvent::MessageUpdate { event, .. } => match event {
                core::AssistantStreamEvent::TextDelta { delta, .. } => {
                    let _ = emitter.event("assistant.text.delta", &json!({"delta": delta}));
                }
                // Core thinking deltas may contain hidden chain-of-thought;
                // the v1 protocol permits provider-authorized summaries only.
                core::AssistantStreamEvent::ThinkingDelta { .. } => {}
                core::AssistantStreamEvent::Error { .. } => failed = true,
                _ => {}
            },
            core::AgentEvent::ToolExecutionStart {
                tool_call_id,
                tool_name,
                args,
            } => {
                let _ = emitter.event(
                    "tool.requested",
                    &json!({
                        "call_id": tool_call_id,
                        "tool": tool_name,
                        "arguments": redact_tool_arguments(&tool_name, &args),
                    }),
                );
                let _ = emitter.event(
                    "tool.authorized",
                    &json!({"call_id": tool_call_id, "tool": tool_name}),
                );
                let _ = emitter.event(
                    "tool.started",
                    &json!({"call_id": tool_call_id, "tool": tool_name}),
                );
            }
            core::AgentEvent::ToolExecutionDenied {
                tool_call_id,
                tool_name,
                reason,
            } => {
                let _ = emitter.event(
                    "tool.denied",
                    &json!({"call_id": tool_call_id, "tool": tool_name, "reason": reason}),
                );
            }
            core::AgentEvent::ToolExecutionUpdate {
                tool_call_id,
                tool_name,
                partial_result,
            } => {
                let _ = emitter.event(
                    "tool.output.delta",
                    &json!({"call_id": tool_call_id, "tool": tool_name, "output": content_value(&partial_result.content)}),
                );
            }
            core::AgentEvent::ToolExecutionEnd {
                tool_call_id,
                tool_name,
                result,
                is_error,
            } => {
                let _ = emitter.event(
                    "tool.completed",
                    &json!({"call_id": tool_call_id, "tool": tool_name, "ok": !is_error, "output": content_value(&result.content), "details": result.details}),
                );
            }
            core::AgentEvent::ContextCompacted {
                original_estimate,
                compacted_estimate,
                messages_pruned,
            } => {
                let _ = emitter.event(
                    "context.compacted",
                    &json!({"original_estimate": original_estimate, "compacted_estimate": compacted_estimate, "messages_pruned": messages_pruned}),
                );
            }
            core::AgentEvent::AgentEnd { messages } => final_messages = Some(messages),
            _ => {}
        }
    }

    let usage = final_messages
        .as_deref()
        .map(usage_from_messages)
        .unwrap_or_default();
    if cancel.is_cancelled() && !failed {
        let _ = emitter.event(
            "run.cancelled",
            &protocol::RunCancelled {
                reason: "execution cancelled".into(),
                usage,
            },
        );
        return 130;
    }
    if failed {
        let code = if request.limits.max_turns.is_some_and(|limit| turns > limit) {
            "limit_exceeded"
        } else {
            "provider_unavailable"
        };
        let _ = emitter.failure(code, "execution did not complete successfully", true);
        return 0;
    }
    let _ = emitter.event("usage.updated", &usage);
    let _ = emitter.event(
        "run.completed",
        &protocol::RunOutcome {
            status: protocol::RunStatus::Succeeded,
            stop_reason: "complete".into(),
            usage,
            artifacts: vec![],
        },
    );
    0
}

fn read_request(path: &Path) -> Result<protocol::RunRequest, String> {
    let mut bytes = Vec::new();
    if path == Path::new("-") {
        io::stdin()
            .take(1_048_577)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
    } else {
        std::fs::File::open(path)
            .and_then(|file| file.take(1_048_577).read_to_end(&mut bytes))
            .map_err(|e| format!("{}: {e}", path.display()))?;
    }
    if bytes.len() > 1_048_576 {
        return Err("request exceeds 1 MiB".into());
    }
    let request: protocol::RunRequest =
        serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    request.validate().map_err(|e| e.to_string())?;
    Ok(request)
}

fn preflight(request: &protocol::RunRequest) -> Result<(), String> {
    if !matches!(
        request.model.provider.as_str(),
        "anthropic" | "openai" | "xai"
    ) {
        return Err(format!(
            "unsupported provider {:?}; expected anthropic, openai, or xai",
            request.model.provider
        ));
    }
    if !request
        .grant
        .providers
        .iter()
        .any(|value| value == &request.model.provider)
    {
        return Err(format!(
            "provider {:?} is not granted",
            request.model.provider
        ));
    }
    if !request
        .grant
        .models
        .iter()
        .any(|pattern| glob_match(pattern, &request.model.id))
    {
        return Err(format!("model {:?} is not granted", request.model.id));
    }
    let expiry = DateTime::parse_from_rfc3339(&request.grant.expires_at)
        .map_err(|_| "grant.expires_at must be RFC 3339".to_string())?;
    if expiry.with_timezone(&Utc) <= Utc::now() {
        return Err("execution grant has expired".into());
    }
    if let Some(deadline) = request.limits.deadline.as_deref() {
        let deadline = DateTime::parse_from_rfc3339(deadline)
            .map_err(|_| "limits.deadline must be RFC 3339".to_string())?;
        if deadline.with_timezone(&Utc) <= Utc::now() {
            return Err("execution deadline has expired".into());
        }
    }
    if request.limits.max_turns == Some(0) {
        return Err("limits.max_turns must be greater than zero".into());
    }
    if request.limits.max_output_tokens == Some(0) {
        return Err("limits.max_output_tokens must be greater than zero".into());
    }
    if request.limits.max_input_tokens.is_some() {
        return Err("limits.max_input_tokens is not supported by this runner".into());
    }
    if request.limits.max_cost_micros.is_some() {
        return Err("limits.max_cost_micros is not supported by this runner".into());
    }
    Ok(())
}

fn validate_supported_tools(request: &protocol::RunRequest) -> Result<(), String> {
    for tool in &request.grant.tools.tools {
        match tool.as_str() {
            "read" | "write" | "edit" | "grep" | "find" => {}
            "bash" => return Err("bash grants are not supported until command_rules can be enforced against shell commands".into()),
            "web_fetch" | "web_search" => return Err(format!("{tool} grants are not supported until network destination policy is enforced")),
            "task" => return Err("task grants are not supported because child capabilities cannot yet be narrowed".into()),
            _ => {}
        }
    }
    if request
        .grant
        .tools
        .tools
        .iter()
        .any(|tool| matches!(tool.as_str(), "write" | "edit"))
        && request.grant.write_roots.is_empty()
    {
        return Err("write and edit tools require a non-empty write_roots grant".into());
    }
    if request
        .grant
        .tools
        .tools
        .iter()
        .any(|tool| matches!(tool.as_str(), "read" | "grep" | "find"))
        && request.grant.read_roots.is_empty()
    {
        return Err("read, grep, and find tools require a non-empty read_roots grant".into());
    }
    Ok(())
}

fn workspace(request: &protocol::RunRequest) -> Result<PathBuf, String> {
    let configured = request.context.get("workspace").and_then(Value::as_str);
    let candidate = configured
        .map(PathBuf::from)
        .or_else(|| request.grant.write_roots.first().map(PathBuf::from))
        .or_else(|| request.grant.read_roots.first().map(PathBuf::from))
        .ok_or_else(|| "context.workspace or a granted root is required".to_string())?;
    let canonical = std::fs::canonicalize(&candidate)
        .map_err(|e| format!("invalid workspace {}: {e}", candidate.display()))?;
    let roots = request
        .grant
        .read_roots
        .iter()
        .chain(&request.grant.write_roots);
    let allowed = roots
        .filter_map(|root| std::fs::canonicalize(root).ok())
        .any(|root| canonical.starts_with(root));
    if !allowed {
        return Err("workspace is outside the granted roots".into());
    }
    Ok(canonical)
}

fn model_config(request: &protocol::RunRequest) -> ModelConfig {
    let mut config = if let Some(config) = ModelRegistry::new().get(&request.model.id) {
        config.clone()
    } else {
        let (provider, base_url, key_env) = match request.model.provider.as_str() {
            "anthropic" => (ProviderType::Anthropic, String::new(), "ANTHROPIC_API_KEY"),
            "xai" => (
                ProviderType::OpenAi,
                "https://api.x.ai/v1".into(),
                "XAI_API_KEY",
            ),
            _ => (ProviderType::OpenAi, String::new(), "OPENAI_API_KEY"),
        };
        ModelConfig {
            id: request.model.id.clone(),
            provider,
            model_id: request.model.id.clone(),
            base_url,
            api_key_env: Some(key_env.into()),
            context_window: 200_000,
            max_tokens: 8_192,
            thinking: false,
            server_tools: None,
            llama_cpp: None,
        }
    };
    if let Some(requested) = request
        .limits
        .max_output_tokens
        .and_then(|value| usize::try_from(value).ok())
    {
        config.max_tokens = config.max_tokens.min(requested);
    }
    config
}

fn resolve_credential(
    request: &protocol::RunRequest,
    model: &ModelConfig,
) -> Result<String, String> {
    if let Some(reference) = request.credential_ref.as_deref() {
        let name = reference
            .strip_prefix("env:")
            .ok_or_else(|| "only env: credential references are supported".to_string())?;
        let expected = model.api_key_env.as_deref().ok_or_else(|| {
            "the selected provider has no credential environment variable".to_string()
        })?;
        if name != expected {
            return Err(format!(
                "credential reference {name:?} is not valid for provider {:?}",
                request.model.provider
            ));
        }
        return std::env::var(name)
            .map_err(|_| format!("credential environment variable {name} is unavailable"))
            .and_then(|value| {
                if value.is_empty() {
                    Err(format!("credential environment variable {name} is empty"))
                } else {
                    Ok(value)
                }
            });
    }
    ModelRegistry::resolve_api_key(model).map_err(|e| e.to_string())
}

fn content_value(content: &[core::Content]) -> Value {
    serde_json::to_value(content).unwrap_or(Value::Null)
}

fn redact_tool_arguments(tool_name: &str, arguments: &Value) -> Value {
    // Mutation payloads routinely contain source, credentials, or generated
    // secrets. The protocol event is an audit record, not a second copy of
    // potentially sensitive file contents.
    if matches!(tool_name, "write" | "edit") {
        let path = arguments
            .get("path")
            .or_else(|| arguments.get("file_path"))
            .cloned()
            .unwrap_or(Value::Null);
        return json!({"path": path, "content_redacted": true});
    }
    redact_value(arguments)
}

fn redact_value(value: &Value) -> Value {
    match value {
        Value::Object(values) => Value::Object(
            values
                .iter()
                .map(|(key, value)| {
                    let normalized = key.to_ascii_lowercase();
                    let sensitive = ["key", "token", "secret", "password", "authorization"]
                        .iter()
                        .any(|needle| normalized.contains(needle));
                    (
                        key.clone(),
                        if sensitive {
                            Value::String("[REDACTED]".into())
                        } else {
                            redact_value(value)
                        },
                    )
                })
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.iter().map(redact_value).collect()),
        _ => value.clone(),
    }
}

fn usage_from_messages(messages: &[core::Message]) -> protocol::Usage {
    messages
        .iter()
        .fold(protocol::Usage::default(), |mut total, message| {
            if let core::Message::Assistant { usage, .. } = message {
                total.input_tokens += usage.input;
                total.output_tokens += usage.output;
                total.cache_read_tokens =
                    Some(total.cache_read_tokens.unwrap_or(0) + usage.cache_read);
                total.cache_write_tokens =
                    Some(total.cache_write_tokens.unwrap_or(0) + usage.cache_write);
            }
            total
        })
}

fn install_cancellation(cancel: CancellationToken, deadline: Option<DateTime<Utc>>) {
    let signal_cancel = cancel.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        signal_cancel.cancel();
    });
    #[cfg(unix)]
    {
        let signal_cancel = cancel.clone();
        tokio::spawn(async move {
            if let Ok(mut signal) =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            {
                signal.recv().await;
                signal_cancel.cancel();
            }
        });
    }
    if let Some(deadline) = deadline {
        let delay = (deadline - Utc::now()).to_std().unwrap_or_default();
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            cancel.cancel();
        });
    }
}

fn effective_deadline(requested: Option<&str>, grant_expiry: &str) -> Option<DateTime<Utc>> {
    let requested = requested
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.with_timezone(&Utc));
    let grant = DateTime::parse_from_rfc3339(grant_expiry)
        .ok()
        .map(|value| value.with_timezone(&Utc));
    match (requested, grant) {
        (Some(requested), Some(grant)) => Some(requested.min(grant)),
        (requested, grant) => requested.or(grant),
    }
}

fn glob_match(pattern: &str, value: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == value,
        Some((prefix, suffix)) => value.starts_with(prefix) && value.ends_with(suffix),
    }
}

struct PathGrantHook {
    cwd: PathBuf,
    read_roots: Vec<PathBuf>,
    write_roots: Vec<PathBuf>,
}

impl PathGrantHook {
    fn new(request: &protocol::RunRequest, cwd: &Path) -> Self {
        Self {
            cwd: cwd.to_path_buf(),
            read_roots: canonical_roots(&request.grant.read_roots),
            write_roots: canonical_roots(&request.grant.write_roots),
        }
    }

    fn allowed(&self, path: &str, roots: &[PathBuf]) -> bool {
        let path = Path::new(path);
        let candidate = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.cwd.join(path)
        };
        let checked = if candidate.exists() {
            std::fs::canonicalize(&candidate).ok()
        } else {
            candidate
                .parent()
                .and_then(|parent| std::fs::canonicalize(parent).ok())
                .and_then(|parent| candidate.file_name().map(|name| parent.join(name)))
        };
        checked.is_some_and(|path| roots.iter().any(|root| path.starts_with(root)))
    }
}

#[async_trait]
impl rho_core::hooks::PreToolUseHook for PathGrantHook {
    fn name(&self) -> &str {
        "protocol-path-grant"
    }

    async fn execute(
        &self,
        tool_name: &str,
        args: &Value,
        _cancel: CancellationToken,
    ) -> rho_core::hooks::PreToolUseResult {
        let (roots, path) = match tool_name {
            "write" => (&self.write_roots, args.get("path").and_then(Value::as_str)),
            "edit" => (
                &self.write_roots,
                args.get("file_path")
                    .or_else(|| args.get("path"))
                    .and_then(Value::as_str),
            ),
            "read" | "grep" => (&self.read_roots, args.get("path").and_then(Value::as_str)),
            "find" => (&self.read_roots, Some(".")),
            _ => {
                return rho_core::hooks::PreToolUseResult::Deny {
                    reason: format!("tool {tool_name:?} has no protocol path policy"),
                }
            }
        };
        let Some(path) = path else {
            return rho_core::hooks::PreToolUseResult::Deny {
                reason: format!("tool {tool_name:?} did not supply a path for authorization"),
            };
        };
        if self.allowed(path, roots) {
            rho_core::hooks::PreToolUseResult::Allow
        } else {
            rho_core::hooks::PreToolUseResult::Deny {
                reason: format!("path {path:?} is outside the execution grant"),
            }
        }
    }
}

fn canonical_roots(roots: &[String]) -> Vec<PathBuf> {
    roots
        .iter()
        .filter_map(|root| std::fs::canonicalize(root).ok())
        .collect()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

struct Emitter {
    run_id: String,
    seq: u64,
}

impl Emitter {
    fn new(run_id: String) -> Self {
        Self { run_id, seq: 0 }
    }

    fn event<T: Serialize>(&mut self, event_type: &str, data: &T) -> io::Result<()> {
        self.seq += 1;
        let event = protocol::RunEvent::new(
            &self.run_id,
            self.seq,
            Utc::now().to_rfc3339(),
            event_type,
            data,
        )
        .map_err(io::Error::other)?;
        let mut stdout = io::stdout().lock();
        serde_json::to_writer(&mut stdout, &event).map_err(io::Error::other)?;
        stdout.write_all(b"\n")?;
        stdout.flush()
    }

    fn failure(
        &mut self,
        code: &str,
        message: impl Into<String>,
        retryable: bool,
    ) -> io::Result<()> {
        self.event(
            "run.failed",
            &protocol::RunFailure {
                code: code.into(),
                message: message.into(),
                retryable,
                retry_after_ms: None,
                details: Default::default(),
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rho_core::hooks::PreToolUseHook;

    fn hosted_request(provider: &str, model: &str) -> protocol::RunRequest {
        protocol::RunRequest::new(
            "run",
            protocol::ModelRef {
                provider: provider.into(),
                id: model.into(),
            },
            vec![],
            protocol::ExecutionGrant {
                grant_id: "g".into(),
                expires_at: "2999-01-01T00:00:00Z".into(),
                providers: vec![provider.into()],
                models: vec![model.into()],
                tools: Default::default(),
                read_roots: vec![],
                write_roots: vec![],
                network: Default::default(),
                witness: None,
                command_policy_ref: None,
                command_rules: vec![],
            },
        )
    }

    #[test]
    fn wildcard_grants_match_prefix_and_suffix() {
        assert!(glob_match("claude-*", "claude-sonnet-4-5"));
        assert!(glob_match("*-mini", "gpt-5-mini"));
        assert!(!glob_match("claude-*", "gpt-5"));
    }

    #[test]
    fn system_messages_are_rejected_in_favor_of_top_level_system() {
        let message = protocol::Message {
            role: protocol::MessageRole::System,
            content: vec![],
        };
        assert!(rho_core::protocol::messages_from_protocol(&[message], 0)
            .unwrap_err()
            .to_string()
            .contains("RunRequest.system"));
    }

    #[test]
    fn granular_effects_fail_closed() {
        let tools = |name: &str| {
            protocol::RunRequest::new(
                "run",
                protocol::ModelRef {
                    provider: "anthropic".into(),
                    id: "claude-test".into(),
                },
                vec![],
                protocol::ExecutionGrant {
                    grant_id: "g".into(),
                    expires_at: "2999-01-01T00:00:00Z".into(),
                    providers: vec!["anthropic".into()],
                    models: vec!["claude-*".into()],
                    tools: protocol::ToolGrant {
                        tools: vec![name.into()],
                    },
                    read_roots: vec!["/tmp".into()],
                    write_roots: vec!["/tmp".into()],
                    network: Default::default(),
                    witness: None,
                    command_policy_ref: None,
                    command_rules: vec![],
                },
            )
        };
        assert!(validate_supported_tools(&tools("bash"))
            .unwrap_err()
            .contains("command_rules"));
        assert!(validate_supported_tools(&tools("web_fetch"))
            .unwrap_err()
            .contains("network"));
        assert!(validate_supported_tools(&tools("task"))
            .unwrap_err()
            .contains("child capabilities"));
    }

    #[test]
    fn path_hook_distinguishes_read_and_write_roots() {
        let temp = tempfile::tempdir().unwrap();
        let read = temp.path().join("read");
        let write = temp.path().join("write");
        std::fs::create_dir_all(&read).unwrap();
        std::fs::create_dir_all(&write).unwrap();
        let hook = PathGrantHook {
            cwd: temp.path().into(),
            read_roots: vec![std::fs::canonicalize(&read).unwrap()],
            write_roots: vec![std::fs::canonicalize(&write).unwrap()],
        };
        assert!(hook.allowed(read.join("a").to_str().unwrap(), &hook.read_roots));
        assert!(!hook.allowed(read.join("a").to_str().unwrap(), &hook.write_roots));
        assert!(hook.allowed(write.join("a").to_str().unwrap(), &hook.write_roots));
    }

    #[tokio::test]
    async fn edit_authorizes_file_path_not_the_workspace_default() {
        let temp = tempfile::tempdir().unwrap();
        let write = temp.path().join("write");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(&write).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let hook = PathGrantHook {
            cwd: write.clone(),
            read_roots: vec![],
            write_roots: vec![std::fs::canonicalize(&write).unwrap()],
        };
        let result = hook
            .execute(
                "edit",
                &json!({"file_path": outside.join("secret")}),
                CancellationToken::new(),
            )
            .await;
        assert!(matches!(
            result,
            rho_core::hooks::PreToolUseResult::Deny { .. }
        ));
    }

    #[test]
    fn unsupported_limits_fail_closed() {
        let request_with = |field: &str| {
            let mut request = protocol::RunRequest::new(
                "run",
                protocol::ModelRef {
                    provider: "anthropic".into(),
                    id: "claude-test".into(),
                },
                vec![],
                protocol::ExecutionGrant {
                    grant_id: "g".into(),
                    expires_at: "2999-01-01T00:00:00Z".into(),
                    providers: vec!["anthropic".into()],
                    models: vec!["claude-*".into()],
                    tools: Default::default(),
                    read_roots: vec![],
                    write_roots: vec![],
                    network: Default::default(),
                    witness: None,
                    command_policy_ref: None,
                    command_rules: vec![],
                },
            );
            match field {
                "input" => request.limits.max_input_tokens = Some(1),
                "cost" => request.limits.max_cost_micros = Some(1),
                _ => unreachable!(),
            }
            request
        };
        assert!(preflight(&request_with("input"))
            .unwrap_err()
            .contains("not supported"));
        assert!(preflight(&request_with("cost"))
            .unwrap_err()
            .contains("not supported"));
    }

    #[test]
    fn grant_expiry_caps_requested_deadline() {
        let effective =
            effective_deadline(Some("2999-01-02T00:00:00Z"), "2999-01-01T00:00:00Z").unwrap();
        assert_eq!(effective.to_rfc3339(), "2999-01-01T00:00:00+00:00");
    }

    #[test]
    fn tool_event_arguments_redact_secrets_and_mutation_content() {
        assert_eq!(
            redact_tool_arguments(
                "read",
                &json!({"path": "a", "nested": {"api_token": "secret"}}),
            ),
            json!({"path": "a", "nested": {"api_token": "[REDACTED]"}})
        );
        assert_eq!(
            redact_tool_arguments("write", &json!({"path": "a", "content": "private source"}),),
            json!({"path": "a", "content_redacted": true})
        );
    }

    #[test]
    fn documented_model_aliases_resolve_to_provider_wire_ids() {
        let claude = model_config(&hosted_request("anthropic", "claude-sonnet"));
        assert_eq!(claude.model_id, "claude-sonnet-4-6");
        assert_eq!(claude.provider, ProviderType::Anthropic);

        let openai = model_config(&hosted_request("openai", "gpt-5.4"));
        assert_eq!(openai.model_id, "gpt-5.4");
        assert_eq!(openai.provider, ProviderType::OpenAi);

        let grok = model_config(&hosted_request("xai", "grok-2"));
        assert_eq!(grok.model_id, "grok-2-1212");
        assert_eq!(grok.provider, ProviderType::OpenAi);
        assert_eq!(grok.base_url, "https://api.x.ai/v1");
    }

    #[test]
    fn raw_model_ids_remain_supported_and_output_limit_is_capped() {
        let mut request = hosted_request("xai", "grok-future");
        request.limits.max_output_tokens = Some(999_999);
        let config = model_config(&request);
        assert_eq!(config.model_id, "grok-future");
        assert_eq!(config.provider, ProviderType::OpenAi);
        assert_eq!(config.max_tokens, 8_192);

        request.limits.max_output_tokens = Some(2_048);
        assert_eq!(model_config(&request).max_tokens, 2_048);
    }
}
