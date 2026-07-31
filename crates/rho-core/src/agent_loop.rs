use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio_util::sync::CancellationToken;

use crate::{
    event_stream::{EventStream, EventStreamConsumer, EventStreamProducer},
    provider_types::{StreamContext, StreamFn, StreamOptions},
    tool::AgentTool,
    types::*,
};

/// When tool count exceeds this, deferrable tools send only their name to the API.
pub const TOOL_DEFER_THRESHOLD: usize = 15;

pub struct AgentLoopConfig {
    pub model: Model,
    pub api_key: String,
    pub system_prompt: String,
    pub tools: Vec<Arc<dyn AgentTool>>,
    pub thinking: ThinkingLevel,
    pub max_tokens: Option<usize>,
    pub stream_fn: StreamFn,
    pub get_steering_messages: Option<Box<dyn Fn() -> Vec<Message> + Send + Sync>>,
    pub get_follow_up_messages: Option<Box<dyn Fn() -> Vec<Message> + Send + Sync>>,
    pub transform_messages: Option<Box<crate::compaction::MessageTransform>>,
    pub post_tools_hooks: Vec<Arc<dyn crate::hooks::PostToolsHook>>,
    pub pre_tool_hooks: Vec<Arc<dyn crate::hooks::PreToolUseHook>>,
    pub lifecycle_hooks: Vec<Arc<dyn crate::hooks::LifecycleHook>>,
    /// Shared reference for cache-optimized subagent forking.
    /// Updated after each assistant response so TaskTool can serialize parent context.
    pub shared_messages: Option<Arc<tokio::sync::RwLock<Vec<Message>>>>,
}

pub fn agent_loop(
    prompts: Vec<Message>,
    config: AgentLoopConfig,
    cancel: CancellationToken,
) -> EventStreamConsumer<AgentEvent, Vec<Message>> {
    let (producer, consumer) = EventStream::<AgentEvent, Vec<Message>>::new().split();

    tokio::spawn(async move {
        run_loop(prompts, config, cancel, producer).await;
    });

    consumer
}

async fn run_loop(
    prompts: Vec<Message>,
    config: AgentLoopConfig,
    cancel: CancellationToken,
    mut stream: EventStreamProducer<AgentEvent, Vec<Message>>,
) {
    let _ = stream.push(AgentEvent::AgentStart).await;

    // Fire SessionStart lifecycle hooks
    for hook in config
        .lifecycle_hooks
        .iter()
        .filter(|h| h.event() == crate::hooks::HookEvent::SessionStart)
    {
        let hook_result = tokio::time::timeout(hook.timeout(), hook.execute(cancel.clone())).await;
        let result = match hook_result {
            Ok(r) => r,
            Err(_) => crate::hooks::LifecycleHookResult {
                steering_message: None,
                success: false,
                summary: format!("{}: timed out", hook.name()),
            },
        };
        let _ = stream
            .push(AgentEvent::LifecycleHookEnd {
                hook_name: hook.name().to_string(),
                event: "session_start".into(),
                success: result.success,
            })
            .await;
    }

    let mut messages = prompts;

    'outer: loop {
        if cancel.is_cancelled() {
            break;
        }

        // === Inner loop: assistant response -> tool execution -> repeat ===
        loop {
            if cancel.is_cancelled() {
                break 'outer;
            }

            let _ = stream.push(AgentEvent::TurnStart).await;

            // 1. Build tool defs and stream assistant response
            // Build tool defs, applying deferral logic when tool count exceeds threshold
            let should_defer = config.tools.len() > TOOL_DEFER_THRESHOLD;
            let previously_used: std::collections::HashSet<String> = if should_defer {
                messages
                    .iter()
                    .filter_map(|m| {
                        if let Message::Assistant { content, .. } = m {
                            content
                                .iter()
                                .filter_map(|c| {
                                    if let Content::ToolCall { name, .. } = c {
                                        Some(name.clone())
                                    } else {
                                        None
                                    }
                                })
                                .next()
                        } else {
                            None
                        }
                    })
                    .collect()
            } else {
                std::collections::HashSet::new()
            };

            let tool_defs: Vec<ToolDef> = config
                .tools
                .iter()
                .map(|t| {
                    let deferred =
                        should_defer && t.is_deferrable() && !previously_used.contains(t.name());
                    ToolDef {
                        name: t.name().to_string(),
                        description: if deferred {
                            String::new()
                        } else {
                            t.description()
                        },
                        parameters: if deferred {
                            serde_json::json!({"type": "object", "properties": {}})
                        } else {
                            t.parameters_schema()
                        },
                        deferred,
                    }
                })
                .collect();

            // Apply transform_messages hook (e.g., compaction) before sending to LLM
            let effective_messages = if let Some(ref transform) = config.transform_messages {
                let (transformed, compaction) = transform(&messages, &config.model);
                if let Some(result) = compaction {
                    let _ = stream
                        .push(AgentEvent::ContextCompacted {
                            original_estimate: result.original_estimate,
                            compacted_estimate: result.compacted_estimate,
                            messages_pruned: result.messages_pruned,
                        })
                        .await;
                }
                transformed
            } else {
                messages.clone()
            };

            let assistant_msg =
                stream_assistant_response(&config, &effective_messages, &tool_defs, &stream).await;

            let _ = stream
                .push(AgentEvent::MessageEnd {
                    message: assistant_msg.clone(),
                })
                .await;

            // 2. Extract tool calls and stop reason
            let (tool_calls, stop_reason) = extract_tool_calls_and_stop(&assistant_msg);
            messages.push(assistant_msg.clone());

            // Update shared messages for cache-optimized subagent forking
            if let Some(ref shared) = config.shared_messages {
                *shared.write().await = messages.clone();
            }

            if tool_calls.is_empty() {
                let _ = stream
                    .push(AgentEvent::TurnEnd {
                        message: assistant_msg,
                        tool_results: vec![],
                    })
                    .await;

                if matches!(stop_reason, StopReason::Error | StopReason::Aborted) {
                    break 'outer;
                }
                break; // break inner, check follow-up
            }

            // 3. Execute tool calls (parallel for concurrent-safe, sequential otherwise)
            let mut tool_results = vec![];
            let groups = group_tool_calls(&tool_calls, &config.tools);

            let mut steered = false;
            for (group_idx, group) in groups.iter().enumerate() {
                if cancel.is_cancelled() {
                    break 'outer;
                }

                // Check steering between groups (not before first one)
                if group_idx > 0 {
                    if let Some(ref get_steering) = config.get_steering_messages {
                        let steering = get_steering();
                        if !steering.is_empty() {
                            // Skip remaining tool calls across all remaining groups
                            for remaining_group in &groups[group_idx..] {
                                for (id, name, _) in remaining_group.calls() {
                                    let skip_result = Message::ToolResult {
                                        tool_call_id: id.clone(),
                                        tool_name: name.clone(),
                                        content: vec![Content::Text {
                                            text: "Tool execution skipped due to steering".into(),
                                        }],
                                        is_error: true,
                                        timestamp: now_ms(),
                                    };
                                    tool_results.push(skip_result.clone());
                                    messages.push(skip_result);
                                }
                            }
                            messages.extend(steering);
                            steered = true;
                            break;
                        }
                    }
                }

                match group {
                    ToolGroup::Sequential(id, name, args) => {
                        // Run pre-tool hooks
                        let mut effective_args = args.clone();
                        let denied = run_pre_tool_hooks(
                            &config.pre_tool_hooks,
                            id,
                            name,
                            &mut effective_args,
                            &cancel,
                            &stream,
                        )
                        .await;

                        if let Some(reason) = denied {
                            // Tool was denied by a pre-tool hook
                            let _ = stream
                                .push(AgentEvent::ToolExecutionDenied {
                                    tool_call_id: id.clone(),
                                    tool_name: name.clone(),
                                    reason: reason.clone(),
                                })
                                .await;
                            let tool_result_msg = Message::ToolResult {
                                tool_call_id: id.clone(),
                                tool_name: name.clone(),
                                content: vec![Content::Text {
                                    text: format!("Tool execution denied: {}", reason),
                                }],
                                is_error: true,
                                timestamp: now_ms(),
                            };
                            tool_results.push(tool_result_msg.clone());
                            messages.push(tool_result_msg);
                            continue;
                        }

                        let _ = stream
                            .push(AgentEvent::ToolExecutionStart {
                                tool_call_id: id.clone(),
                                tool_name: name.clone(),
                                args: effective_args.clone(),
                            })
                            .await;

                        let (result, is_error) = execute_tool_call(
                            &config.tools,
                            id,
                            name,
                            effective_args,
                            cancel.clone(),
                        )
                        .await;

                        let tool_result_msg = Message::ToolResult {
                            tool_call_id: id.clone(),
                            tool_name: name.clone(),
                            content: result.content.clone(),
                            is_error,
                            timestamp: now_ms(),
                        };
                        tool_results.push(tool_result_msg.clone());
                        messages.push(tool_result_msg);

                        let _ = stream
                            .push(AgentEvent::ToolExecutionEnd {
                                tool_call_id: id.clone(),
                                tool_name: name.clone(),
                                result,
                                is_error,
                            })
                            .await;
                    }
                    ToolGroup::Parallel(calls) => {
                        // Emit all Start events
                        for (id, name, args) in calls {
                            let _ = stream
                                .push(AgentEvent::ToolExecutionStart {
                                    tool_call_id: id.clone(),
                                    tool_name: name.clone(),
                                    args: args.clone(),
                                })
                                .await;
                        }

                        // Execute all concurrently
                        let futures: Vec<_> = calls
                            .iter()
                            .map(|(id, name, args)| {
                                let tools = config.tools.clone();
                                let cancel = cancel.clone();
                                let id = id.clone();
                                let name = name.clone();
                                let args = args.clone();
                                async move {
                                    let (result, is_error) =
                                        execute_tool_call(&tools, &id, &name, args, cancel).await;
                                    (id, name, result, is_error)
                                }
                            })
                            .collect();

                        let results = futures::future::join_all(futures).await;

                        // Emit all End events in original order
                        for (id, name, result, is_error) in results {
                            let tool_result_msg = Message::ToolResult {
                                tool_call_id: id.clone(),
                                tool_name: name.clone(),
                                content: result.content.clone(),
                                is_error,
                                timestamp: now_ms(),
                            };
                            tool_results.push(tool_result_msg.clone());
                            messages.push(tool_result_msg);

                            let _ = stream
                                .push(AgentEvent::ToolExecutionEnd {
                                    tool_call_id: id.clone(),
                                    tool_name: name.clone(),
                                    result,
                                    is_error,
                                })
                                .await;
                        }
                    }
                }
            }
            // If we broke due to steering, skip post-tools hooks
            if steered {
                let _ = stream
                    .push(AgentEvent::TurnEnd {
                        message: assistant_msg,
                        tool_results,
                    })
                    .await;
                continue;
            }

            // Run post-tools hooks if tools were called
            if !tool_calls.is_empty() && !config.post_tools_hooks.is_empty() {
                let tool_names: Vec<String> =
                    tool_calls.iter().map(|(_, name, _)| name.clone()).collect();
                for hook in &config.post_tools_hooks {
                    if cancel.is_cancelled() {
                        break 'outer;
                    }
                    let _ = stream
                        .push(AgentEvent::PostToolsHookStart {
                            hook_name: hook.name().to_string(),
                        })
                        .await;

                    let hook_result = tokio::time::timeout(
                        hook.timeout(),
                        hook.execute(&tool_names, cancel.clone()),
                    )
                    .await;

                    let result = match hook_result {
                        Ok(r) => r,
                        Err(_) => crate::hooks::PostToolsHookResult {
                            steering_message: None,
                            success: false,
                            summary: format!("{}: timed out", hook.name()),
                        },
                    };

                    let _ = stream
                        .push(AgentEvent::PostToolsHookEnd {
                            hook_name: hook.name().to_string(),
                            success: result.success,
                            summary: result.summary,
                        })
                        .await;

                    // Inject steering message as User message (NOT in tool results)
                    if let Some(steering) = result.steering_message {
                        messages.push(Message::User {
                            content: UserContent::Text(steering),
                            timestamp: now_ms(),
                        });
                    }
                }
            }

            // Fire TurnEnd lifecycle hooks
            for hook in config
                .lifecycle_hooks
                .iter()
                .filter(|h| h.event() == crate::hooks::HookEvent::TurnEnd)
            {
                if cancel.is_cancelled() {
                    break 'outer;
                }
                let hook_result =
                    tokio::time::timeout(hook.timeout(), hook.execute(cancel.clone())).await;
                let result = match hook_result {
                    Ok(r) => r,
                    Err(_) => crate::hooks::LifecycleHookResult {
                        steering_message: None,
                        success: false,
                        summary: format!("{}: timed out", hook.name()),
                    },
                };
                let _ = stream
                    .push(AgentEvent::LifecycleHookEnd {
                        hook_name: hook.name().to_string(),
                        event: "turn_end".into(),
                        success: result.success,
                    })
                    .await;
                if let Some(steering) = result.steering_message {
                    messages.push(Message::User {
                        content: UserContent::Text(steering),
                        timestamp: now_ms(),
                    });
                }
            }

            let _ = stream
                .push(AgentEvent::TurnEnd {
                    message: assistant_msg,
                    tool_results,
                })
                .await;

            // Continue inner loop to send tool results back to LLM
        }

        // === Check for follow-up messages ===
        if let Some(ref get_follow_up) = config.get_follow_up_messages {
            let follow_ups = get_follow_up();
            if !follow_ups.is_empty() {
                messages.extend(follow_ups);
                continue 'outer;
            }
        }

        break 'outer;
    }

    let _ = stream
        .push(AgentEvent::AgentEnd {
            messages: messages.clone(),
        })
        .await;
    stream.end(Some(messages));
}

async fn stream_assistant_response(
    config: &AgentLoopConfig,
    messages: &[Message],
    tool_defs: &[ToolDef],
    stream: &EventStreamProducer<AgentEvent, Vec<Message>>,
) -> Message {
    let context = StreamContext {
        messages: messages.to_vec(),
        tools: tool_defs.to_vec(),
    };
    let options = StreamOptions {
        api_key: config.api_key.clone(),
        system_prompt: Some(config.system_prompt.clone()),
        max_tokens: config.max_tokens,
        thinking: config.thinking,
    };

    let mut assistant_stream = (config.stream_fn)(&config.model, context, options);

    // Build up partial message from events
    let mut content_blocks: Vec<Content> = vec![];
    let model_str = config.model.id.clone();
    let usage = Usage::default();
    let mut stop_reason = StopReason::Stop;
    let mut first_event = true;

    while let Some(event) = assistant_stream.next().await {
        match &event {
            AssistantStreamEvent::TextStart { .. } => {
                content_blocks.push(Content::Text {
                    text: String::new(),
                });
            }
            AssistantStreamEvent::TextDelta { delta, .. } => {
                if let Some(Content::Text { ref mut text }) = content_blocks.last_mut() {
                    text.push_str(delta);
                }
            }
            AssistantStreamEvent::ThinkingStart { .. } => {
                content_blocks.push(Content::Thinking {
                    thinking: String::new(),
                });
            }
            AssistantStreamEvent::ThinkingDelta { delta, .. } => {
                if let Some(Content::Thinking { ref mut thinking }) = content_blocks.last_mut() {
                    thinking.push_str(delta);
                }
            }
            AssistantStreamEvent::ToolCallStart { .. } => {
                content_blocks.push(Content::ToolCall {
                    id: String::new(),
                    name: String::new(),
                    arguments: serde_json::Value::Null,
                });
            }
            AssistantStreamEvent::ToolCallEnd { tool_call, .. } => {
                if let Some(last) = content_blocks.last_mut() {
                    *last = tool_call.clone();
                }
            }
            AssistantStreamEvent::Done { stop_reason: sr } => {
                stop_reason = sr.clone();
            }
            AssistantStreamEvent::Error { stop_reason: sr } => {
                stop_reason = sr.clone();
            }
            _ => {}
        }

        let partial_msg = Message::Assistant {
            content: content_blocks.clone(),
            model: model_str.clone(),
            usage: usage.clone(),
            stop_reason: stop_reason.clone(),
            timestamp: now_ms(),
        };

        if first_event {
            let _ = stream
                .push(AgentEvent::MessageStart {
                    message: partial_msg.clone(),
                })
                .await;
            first_event = false;
        }

        let _ = stream
            .push(AgentEvent::MessageUpdate {
                message: partial_msg,
                event,
            })
            .await;
    }

    // Try to get the final assembled message from the stream
    if let Some(final_msg) = assistant_stream.result().await {
        final_msg
    } else {
        // Fallback: build from accumulated state
        Message::Assistant {
            content: content_blocks,
            model: model_str,
            usage,
            stop_reason,
            timestamp: now_ms(),
        }
    }
}

fn extract_tool_calls_and_stop(
    msg: &Message,
) -> (Vec<(String, String, serde_json::Value)>, StopReason) {
    match msg {
        Message::Assistant {
            content,
            stop_reason,
            ..
        } => {
            let calls: Vec<_> = content
                .iter()
                .filter_map(|c| {
                    if let Content::ToolCall {
                        id,
                        name,
                        arguments,
                    } = c
                    {
                        Some((id.clone(), name.clone(), arguments.clone()))
                    } else {
                        None
                    }
                })
                .collect();
            (calls, stop_reason.clone())
        }
        _ => (vec![], StopReason::Stop),
    }
}

/// Run pre-tool-use hooks. Returns Some(reason) if denied, None if allowed.
/// May modify `args` in place if a hook returns AllowWithModifiedInput.
async fn run_pre_tool_hooks(
    hooks: &[Arc<dyn crate::hooks::PreToolUseHook>],
    _tool_call_id: &str,
    tool_name: &str,
    args: &mut serde_json::Value,
    cancel: &CancellationToken,
    stream: &EventStreamProducer<AgentEvent, Vec<Message>>,
) -> Option<String> {
    for hook in hooks {
        let hook_result = tokio::time::timeout(
            hook.timeout(),
            hook.execute(tool_name, args, cancel.clone()),
        )
        .await;

        let result = match hook_result {
            Ok(r) => r,
            Err(_) => {
                // Timeout — allow by default
                let _ = stream
                    .push(AgentEvent::PreToolUseHookEnd {
                        hook_name: hook.name().to_string(),
                        tool_name: tool_name.to_string(),
                        decision: "allow (timeout)".into(),
                    })
                    .await;
                continue;
            }
        };

        match result {
            crate::hooks::PreToolUseResult::Allow => {
                let _ = stream
                    .push(AgentEvent::PreToolUseHookEnd {
                        hook_name: hook.name().to_string(),
                        tool_name: tool_name.to_string(),
                        decision: "allow".into(),
                    })
                    .await;
            }
            crate::hooks::PreToolUseResult::AllowWithModifiedInput(new_args) => {
                *args = new_args;
                let _ = stream
                    .push(AgentEvent::PreToolUseHookEnd {
                        hook_name: hook.name().to_string(),
                        tool_name: tool_name.to_string(),
                        decision: "allow (modified)".into(),
                    })
                    .await;
            }
            crate::hooks::PreToolUseResult::Deny { reason } => {
                let _ = stream
                    .push(AgentEvent::PreToolUseHookEnd {
                        hook_name: hook.name().to_string(),
                        tool_name: tool_name.to_string(),
                        decision: "deny".into(),
                    })
                    .await;
                return Some(reason);
            }
        }
    }
    None
}

/// A group of tool calls that can be executed together.
enum ToolGroup {
    /// A single tool that must run sequentially (has side effects).
    Sequential(String, String, serde_json::Value),
    /// Multiple concurrent-safe tools that can run in parallel.
    Parallel(Vec<(String, String, serde_json::Value)>),
}

impl ToolGroup {
    /// Get all (id, name, args) tuples in this group.
    fn calls(&self) -> Vec<(&String, &String, &serde_json::Value)> {
        match self {
            ToolGroup::Sequential(id, name, args) => vec![(id, name, args)],
            ToolGroup::Parallel(calls) => calls
                .iter()
                .map(|(id, name, args)| (id, name, args))
                .collect(),
        }
    }
}

/// Partition tool calls into contiguous groups of parallel (concurrent-safe) and sequential tools.
fn group_tool_calls(
    tool_calls: &[(String, String, serde_json::Value)],
    tools: &[Arc<dyn AgentTool>],
) -> Vec<ToolGroup> {
    let mut groups = Vec::new();
    let mut parallel_buf: Vec<(String, String, serde_json::Value)> = Vec::new();

    for (id, name, args) in tool_calls {
        let is_safe = tools
            .iter()
            .find(|t| t.name() == name)
            .map(|t| t.is_concurrent_safe())
            .unwrap_or(false);

        if is_safe {
            parallel_buf.push((id.clone(), name.clone(), args.clone()));
        } else {
            // Flush any accumulated parallel tools first
            if !parallel_buf.is_empty() {
                if parallel_buf.len() == 1 {
                    let (id, name, args) = parallel_buf.pop().unwrap();
                    groups.push(ToolGroup::Sequential(id, name, args));
                } else {
                    groups.push(ToolGroup::Parallel(std::mem::take(&mut parallel_buf)));
                }
            }
            groups.push(ToolGroup::Sequential(
                id.clone(),
                name.clone(),
                args.clone(),
            ));
        }
    }

    // Flush remaining parallel buffer
    if !parallel_buf.is_empty() {
        if parallel_buf.len() == 1 {
            let (id, name, args) = parallel_buf.pop().unwrap();
            groups.push(ToolGroup::Sequential(id, name, args));
        } else {
            groups.push(ToolGroup::Parallel(std::mem::take(&mut parallel_buf)));
        }
    }

    groups
}

async fn execute_tool_call(
    tools: &[Arc<dyn AgentTool>],
    id: &str,
    name: &str,
    args: serde_json::Value,
    cancel: CancellationToken,
) -> (ToolResult, bool) {
    let tool = tools.iter().find(|t| t.name() == name);

    match tool {
        Some(tool) => match tool.execute(id, args, cancel).await {
            Ok(result) => (result, false),
            Err(e) => {
                let error_result = ToolResult {
                    content: vec![Content::Text {
                        text: format!("Tool error: {}", e),
                    }],
                    details: serde_json::json!({}),
                };
                (error_result, true)
            }
        },
        None => {
            let error_result = ToolResult {
                content: vec![Content::Text {
                    text: format!("Tool '{}' not found", name),
                }],
                details: serde_json::json!({}),
            };
            (error_result, true)
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_types::{AssistantStream, StreamFn};
    use crate::tool::ToolError;
    use async_trait::async_trait;
    use std::sync::atomic::Ordering;

    fn test_model() -> Model {
        Model {
            id: "test-model".into(),
            name: "Test Model".into(),
            provider: "test".into(),
            base_url: "http://localhost".into(),
            reasoning: false,
            context_window: 8192,
            max_tokens: 4096,
        }
    }

    /// Helper: build a multi-call mock stream_fn that returns different responses per call.
    fn mock_stream_fn_multi(
        calls: Vec<(Vec<AssistantStreamEvent>, Message)>,
    ) -> (Arc<std::sync::atomic::AtomicUsize>, StreamFn) {
        let call_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let call_count_clone = call_count.clone();
        let calls = Arc::new(calls);

        let f: StreamFn = Arc::new(
            move |_model: &Model, _ctx: StreamContext, _opts: StreamOptions| {
                let n = call_count_clone.fetch_add(1, Ordering::SeqCst);
                let (events, final_msg) = (*calls)[n].clone();

                let (mut producer, consumer) = AssistantStream::new().split();

                tokio::spawn(async move {
                    for event in events {
                        let _ = producer.push(event).await;
                    }
                    producer.end(Some(final_msg));
                });

                consumer
            },
        );

        (call_count, f)
    }

    /// Helper: build a single-use mock stream_fn from canned events + final Message.
    fn mock_stream_fn(events: Vec<AssistantStreamEvent>, final_msg: Message) -> StreamFn {
        let (_, f) = mock_stream_fn_multi(vec![(events, final_msg)]);
        f
    }

    #[tokio::test]
    async fn test_text_only_response() {
        let final_msg = Message::Assistant {
            content: vec![Content::Text {
                text: "Hello world".into(),
            }],
            model: "test-model".into(),
            usage: Usage::default(),
            stop_reason: StopReason::Stop,
            timestamp: 100,
        };

        let events = vec![
            AssistantStreamEvent::Start,
            AssistantStreamEvent::TextStart { index: 0 },
            AssistantStreamEvent::TextDelta {
                index: 0,
                delta: "Hello ".into(),
            },
            AssistantStreamEvent::TextDelta {
                index: 0,
                delta: "world".into(),
            },
            AssistantStreamEvent::TextEnd {
                index: 0,
                content: "Hello world".into(),
            },
            AssistantStreamEvent::Done {
                stop_reason: StopReason::Stop,
            },
        ];

        let config = AgentLoopConfig {
            model: test_model(),
            api_key: "test-key".into(),
            system_prompt: "You are a test".into(),
            tools: vec![],
            thinking: ThinkingLevel::Off,
            max_tokens: None,
            stream_fn: mock_stream_fn(events, final_msg),
            get_steering_messages: None,
            get_follow_up_messages: None,
            transform_messages: None,
            post_tools_hooks: vec![],
            pre_tool_hooks: vec![],
            lifecycle_hooks: vec![],
            shared_messages: None,
        };

        let prompts = vec![Message::User {
            content: UserContent::Text("hi".into()),
            timestamp: 1,
        }];

        let cancel = CancellationToken::new();
        let mut consumer = agent_loop(prompts, config, cancel);

        let mut agent_events = vec![];
        while let Some(event) = consumer.next().await {
            agent_events.push(event);
        }

        // Verify event sequence
        assert!(matches!(agent_events[0], AgentEvent::AgentStart));
        assert!(matches!(agent_events[1], AgentEvent::TurnStart));

        let has_msg_start = agent_events
            .iter()
            .any(|e| matches!(e, AgentEvent::MessageStart { .. }));
        assert!(has_msg_start);

        let has_msg_end = agent_events
            .iter()
            .any(|e| matches!(e, AgentEvent::MessageEnd { .. }));
        assert!(has_msg_end);

        let has_turn_end = agent_events.iter().any(
            |e| matches!(e, AgentEvent::TurnEnd { tool_results, .. } if tool_results.is_empty()),
        );
        assert!(has_turn_end);

        assert!(matches!(
            agent_events.last().unwrap(),
            AgentEvent::AgentEnd { .. }
        ));
    }

    // Mock tool for testing
    struct MockTool {
        tool_name: String,
        result: ToolResult,
        concurrent_safe: bool,
    }

    impl MockTool {
        fn new(name: &str, output: &str) -> Self {
            Self {
                tool_name: name.into(),
                result: ToolResult {
                    content: vec![Content::Text {
                        text: output.into(),
                    }],
                    details: serde_json::json!({}),
                },
                concurrent_safe: false,
            }
        }

        fn concurrent(mut self) -> Self {
            self.concurrent_safe = true;
            self
        }
    }

    #[async_trait]
    impl AgentTool for MockTool {
        fn name(&self) -> &str {
            &self.tool_name
        }
        fn label(&self) -> String {
            self.tool_name.clone()
        }
        fn description(&self) -> String {
            "A mock tool".into()
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
        fn is_concurrent_safe(&self) -> bool {
            self.concurrent_safe
        }
        async fn execute(
            &self,
            _tool_call_id: &str,
            _params: serde_json::Value,
            _cancel: CancellationToken,
        ) -> Result<ToolResult, ToolError> {
            Ok(self.result.clone())
        }
    }

    #[tokio::test]
    async fn test_tool_call_response() {
        let tool_call_msg = Message::Assistant {
            content: vec![Content::ToolCall {
                id: "call_1".into(),
                name: "mock_tool".into(),
                arguments: serde_json::json!({"key": "value"}),
            }],
            model: "test-model".into(),
            usage: Usage::default(),
            stop_reason: StopReason::ToolUse,
            timestamp: 100,
        };

        let text_msg = Message::Assistant {
            content: vec![Content::Text {
                text: "Done".into(),
            }],
            model: "test-model".into(),
            usage: Usage::default(),
            stop_reason: StopReason::Stop,
            timestamp: 200,
        };

        let (call_count, stream_fn) = mock_stream_fn_multi(vec![
            (
                vec![
                    AssistantStreamEvent::Start,
                    AssistantStreamEvent::ToolCallStart { index: 0 },
                    AssistantStreamEvent::ToolCallEnd {
                        index: 0,
                        tool_call: Content::ToolCall {
                            id: "call_1".into(),
                            name: "mock_tool".into(),
                            arguments: serde_json::json!({"key": "value"}),
                        },
                    },
                    AssistantStreamEvent::Done {
                        stop_reason: StopReason::ToolUse,
                    },
                ],
                tool_call_msg,
            ),
            (
                vec![
                    AssistantStreamEvent::Start,
                    AssistantStreamEvent::TextStart { index: 0 },
                    AssistantStreamEvent::TextDelta {
                        index: 0,
                        delta: "Done".into(),
                    },
                    AssistantStreamEvent::TextEnd {
                        index: 0,
                        content: "Done".into(),
                    },
                    AssistantStreamEvent::Done {
                        stop_reason: StopReason::Stop,
                    },
                ],
                text_msg,
            ),
        ]);

        let mock_tool = Arc::new(MockTool::new("mock_tool", "tool output"));

        let config = AgentLoopConfig {
            model: test_model(),
            api_key: "test-key".into(),
            system_prompt: "You are a test".into(),
            tools: vec![mock_tool],
            thinking: ThinkingLevel::Off,
            max_tokens: None,
            stream_fn,
            get_steering_messages: None,
            get_follow_up_messages: None,
            transform_messages: None,
            post_tools_hooks: vec![],
            pre_tool_hooks: vec![],
            lifecycle_hooks: vec![],
            shared_messages: None,
        };

        let prompts = vec![Message::User {
            content: UserContent::Text("use the tool".into()),
            timestamp: 1,
        }];

        let cancel = CancellationToken::new();
        let mut consumer = agent_loop(prompts, config, cancel);

        let mut agent_events = vec![];
        while let Some(event) = consumer.next().await {
            agent_events.push(event);
        }

        let has_tool_start = agent_events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolExecutionStart { tool_name, .. } if tool_name == "mock_tool"));
        assert!(has_tool_start);

        let has_tool_end = agent_events.iter().any(
            |e| matches!(e, AgentEvent::ToolExecutionEnd { tool_name, is_error, .. } if tool_name == "mock_tool" && !is_error),
        );
        assert!(has_tool_end);

        assert_eq!(call_count.load(Ordering::SeqCst), 2);

        assert!(matches!(
            agent_events.last().unwrap(),
            AgentEvent::AgentEnd { .. }
        ));
    }

    #[tokio::test]
    async fn test_tool_not_found() {
        let tool_call_msg = Message::Assistant {
            content: vec![Content::ToolCall {
                id: "call_1".into(),
                name: "nonexistent_tool".into(),
                arguments: serde_json::json!({}),
            }],
            model: "test-model".into(),
            usage: Usage::default(),
            stop_reason: StopReason::ToolUse,
            timestamp: 100,
        };

        let text_msg = Message::Assistant {
            content: vec![Content::Text {
                text: "Sorry".into(),
            }],
            model: "test-model".into(),
            usage: Usage::default(),
            stop_reason: StopReason::Stop,
            timestamp: 200,
        };

        let (_call_count, stream_fn) = mock_stream_fn_multi(vec![
            (
                vec![
                    AssistantStreamEvent::Start,
                    AssistantStreamEvent::ToolCallStart { index: 0 },
                    AssistantStreamEvent::ToolCallEnd {
                        index: 0,
                        tool_call: Content::ToolCall {
                            id: "call_1".into(),
                            name: "nonexistent_tool".into(),
                            arguments: serde_json::json!({}),
                        },
                    },
                    AssistantStreamEvent::Done {
                        stop_reason: StopReason::ToolUse,
                    },
                ],
                tool_call_msg,
            ),
            (
                vec![
                    AssistantStreamEvent::Start,
                    AssistantStreamEvent::TextStart { index: 0 },
                    AssistantStreamEvent::TextDelta {
                        index: 0,
                        delta: "Sorry".into(),
                    },
                    AssistantStreamEvent::TextEnd {
                        index: 0,
                        content: "Sorry".into(),
                    },
                    AssistantStreamEvent::Done {
                        stop_reason: StopReason::Stop,
                    },
                ],
                text_msg,
            ),
        ]);

        let config = AgentLoopConfig {
            model: test_model(),
            api_key: "test-key".into(),
            system_prompt: "You are a test".into(),
            tools: vec![],
            thinking: ThinkingLevel::Off,
            max_tokens: None,
            stream_fn,
            get_steering_messages: None,
            get_follow_up_messages: None,
            transform_messages: None,
            post_tools_hooks: vec![],
            pre_tool_hooks: vec![],
            lifecycle_hooks: vec![],
            shared_messages: None,
        };

        let prompts = vec![Message::User {
            content: UserContent::Text("use the tool".into()),
            timestamp: 1,
        }];

        let cancel = CancellationToken::new();
        let mut consumer = agent_loop(prompts, config, cancel);

        let mut agent_events = vec![];
        while let Some(event) = consumer.next().await {
            agent_events.push(event);
        }

        let has_error_tool = agent_events.iter().any(
            |e| matches!(e, AgentEvent::ToolExecutionEnd { is_error, tool_name, .. } if *is_error && tool_name == "nonexistent_tool"),
        );
        assert!(has_error_tool);
    }

    #[test]
    fn test_group_tool_calls_all_sequential() {
        let tools: Vec<Arc<dyn AgentTool>> = vec![
            Arc::new(MockTool::new("write", "ok")),
            Arc::new(MockTool::new("bash", "ok")),
        ];
        let calls = vec![
            ("c1".into(), "write".into(), serde_json::json!({})),
            ("c2".into(), "bash".into(), serde_json::json!({})),
        ];
        let groups = group_tool_calls(&calls, &tools);
        assert_eq!(groups.len(), 2);
        assert!(matches!(&groups[0], ToolGroup::Sequential(_, name, _) if name == "write"));
        assert!(matches!(&groups[1], ToolGroup::Sequential(_, name, _) if name == "bash"));
    }

    #[test]
    fn test_group_tool_calls_all_parallel() {
        let tools: Vec<Arc<dyn AgentTool>> = vec![
            Arc::new(MockTool::new("read", "ok").concurrent()),
            Arc::new(MockTool::new("grep", "ok").concurrent()),
            Arc::new(MockTool::new("find", "ok").concurrent()),
        ];
        let calls = vec![
            ("c1".into(), "read".into(), serde_json::json!({})),
            ("c2".into(), "grep".into(), serde_json::json!({})),
            ("c3".into(), "find".into(), serde_json::json!({})),
        ];
        let groups = group_tool_calls(&calls, &tools);
        assert_eq!(groups.len(), 1);
        assert!(matches!(&groups[0], ToolGroup::Parallel(calls) if calls.len() == 3));
    }

    #[test]
    fn test_group_tool_calls_mixed() {
        let tools: Vec<Arc<dyn AgentTool>> = vec![
            Arc::new(MockTool::new("read", "ok").concurrent()),
            Arc::new(MockTool::new("grep", "ok").concurrent()),
            Arc::new(MockTool::new("write", "ok")),
            Arc::new(MockTool::new("find", "ok").concurrent()),
        ];
        let calls = vec![
            ("c1".into(), "read".into(), serde_json::json!({})),
            ("c2".into(), "grep".into(), serde_json::json!({})),
            ("c3".into(), "write".into(), serde_json::json!({})),
            ("c4".into(), "find".into(), serde_json::json!({})),
        ];
        let groups = group_tool_calls(&calls, &tools);
        // [read, grep] -> Parallel, [write] -> Sequential, [find] -> Sequential (single concurrent = sequential)
        assert_eq!(groups.len(), 3);
        assert!(matches!(&groups[0], ToolGroup::Parallel(calls) if calls.len() == 2));
        assert!(matches!(&groups[1], ToolGroup::Sequential(_, name, _) if name == "write"));
        assert!(matches!(&groups[2], ToolGroup::Sequential(_, name, _) if name == "find"));
    }

    #[test]
    fn test_group_tool_calls_single_concurrent_is_sequential() {
        let tools: Vec<Arc<dyn AgentTool>> =
            vec![Arc::new(MockTool::new("read", "ok").concurrent())];
        let calls = vec![("c1".into(), "read".into(), serde_json::json!({}))];
        let groups = group_tool_calls(&calls, &tools);
        assert_eq!(groups.len(), 1);
        // Single concurrent tool should be Sequential (no overhead of join_all)
        assert!(matches!(&groups[0], ToolGroup::Sequential(..)));
    }
}
