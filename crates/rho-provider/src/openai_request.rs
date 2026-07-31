use rho_core::provider_types::{StreamContext, StreamOptions};
use rho_core::types::{Content, Message, Model, ToolDef, UserContent};
use serde_json::{json, Value};

/// Build an OpenAI-compatible chat completions request body.
///
/// `server_tools` are provider-managed tools (e.g. xAI's `web_search`, `x_search`) that
/// are appended to the `tools` array as `{"type": "<name>"}` entries. They run on the
/// provider's servers and do not correspond to local tool definitions.
pub fn build_request_body(
    model: &Model,
    context: &StreamContext,
    options: &StreamOptions,
    server_tools: Option<&[String]>,
) -> Value {
    let mut messages: Vec<Value> = Vec::new();

    // System prompt as first message
    if let Some(ref system) = options.system_prompt {
        messages.push(json!({
            "role": "system",
            "content": system,
        }));
    }

    // Conversation messages
    messages.extend(convert_messages(&context.messages));

    let mut body = json!({
        "model": model.id,
        "max_tokens": options.max_tokens.unwrap_or(model.max_tokens),
        "stream": true,
        "stream_options": { "include_usage": true },
        "messages": messages,
    });

    // Local tool definitions
    let mut tools: Vec<Value> = context.tools.iter().map(convert_tool_def).collect();

    // Provider-managed server-side tools
    if let Some(st) = server_tools {
        for name in st {
            tools.push(json!({ "type": name }));
        }
    }

    if !tools.is_empty() {
        body["tools"] = Value::Array(tools);
    }

    body
}

fn convert_messages(messages: &[Message]) -> Vec<Value> {
    let mut result = Vec::new();

    for msg in messages {
        match msg {
            Message::User { content, .. } => {
                result.push(convert_user_message(content));
            }
            Message::Assistant { content, .. } => {
                result.push(convert_assistant_message(content));
            }
            Message::ToolResult {
                tool_call_id,
                content,
                is_error,
                ..
            } => {
                // Each tool result is a separate "tool" role message in OpenAI format
                let text = extract_text(content);
                let content_str = if *is_error {
                    format!("Error: {}", text)
                } else {
                    text
                };
                result.push(json!({
                    "role": "tool",
                    "tool_call_id": tool_call_id,
                    "content": content_str,
                }));
            }
        }
    }

    result
}

fn convert_user_message(content: &UserContent) -> Value {
    match content {
        UserContent::Text(s) => json!({
            "role": "user",
            "content": s,
        }),
        UserContent::Blocks(blocks) => {
            let content_parts: Vec<Value> = blocks
                .iter()
                .filter_map(|b| match b {
                    Content::Text { text } => Some(json!({
                        "type": "text",
                        "text": text,
                    })),
                    Content::Image { data, mime_type } => Some(json!({
                        "type": "image_url",
                        "image_url": {
                            "url": format!("data:{};base64,{}", mime_type, data),
                        },
                    })),
                    // Skip Thinking blocks in user messages
                    Content::Thinking { .. } | Content::ToolCall { .. } => None,
                })
                .collect();

            json!({
                "role": "user",
                "content": content_parts,
            })
        }
    }
}

fn convert_assistant_message(content: &[Content]) -> Value {
    let mut text_parts = Vec::new();
    let mut tool_calls = Vec::new();

    for block in content {
        match block {
            Content::Text { text } => {
                text_parts.push(text.as_str());
            }
            Content::ToolCall {
                id,
                name,
                arguments,
            } => {
                let args_str = serde_json::to_string(arguments).unwrap_or_else(|_| "{}".into());
                tool_calls.push(json!({
                    "id": id,
                    "type": "function",
                    "function": {
                        "name": name,
                        "arguments": args_str,
                    },
                }));
            }
            // Skip Thinking blocks — not meaningful for OpenAI-compat
            Content::Thinking { .. } | Content::Image { .. } => {}
        }
    }

    let combined_text = text_parts.join("");

    let mut msg = json!({ "role": "assistant" });

    if combined_text.is_empty() {
        msg["content"] = Value::Null;
    } else {
        msg["content"] = json!(combined_text);
    }

    if !tool_calls.is_empty() {
        msg["tool_calls"] = json!(tool_calls);
    }

    msg
}

fn convert_tool_def(tool: &ToolDef) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": tool.name,
            "description": tool.description,
            "parameters": tool.parameters,
        },
    })
}

fn extract_text(content: &[Content]) -> String {
    content
        .iter()
        .filter_map(|c| match c {
            Content::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rho_core::provider_types::{StreamContext, StreamOptions};
    use rho_core::types::{Message, Model, StopReason, ThinkingLevel, ToolDef, Usage, UserContent};

    fn test_model() -> Model {
        Model {
            id: "gpt-4o".into(),
            name: "gpt-4o".into(),
            provider: "openai".into(),
            base_url: String::new(),
            reasoning: false,
            context_window: 128_000,
            max_tokens: 4_096,
        }
    }

    fn now_ms() -> u64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
    }

    #[test]
    fn simple_user_message() {
        let model = test_model();
        let context = StreamContext {
            messages: vec![Message::User {
                content: UserContent::Text("Hello".into()),
                timestamp: now_ms(),
            }],
            tools: vec![],
        };
        let options = StreamOptions {
            api_key: "key".into(),
            system_prompt: None,
            max_tokens: None,
            thinking: ThinkingLevel::Off,
        };

        let body = build_request_body(&model, &context, &options, None);
        assert_eq!(body["model"], "gpt-4o");
        assert_eq!(body["stream"], true);

        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"], "Hello");
    }

    #[test]
    fn system_prompt_prepended() {
        let model = test_model();
        let context = StreamContext {
            messages: vec![Message::User {
                content: UserContent::Text("Hi".into()),
                timestamp: now_ms(),
            }],
            tools: vec![],
        };
        let options = StreamOptions {
            api_key: "key".into(),
            system_prompt: Some("You are helpful.".into()),
            max_tokens: None,
            thinking: ThinkingLevel::Off,
        };

        let body = build_request_body(&model, &context, &options, None);
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "You are helpful.");
        assert_eq!(messages[1]["role"], "user");
    }

    #[test]
    fn assistant_message_with_tool_calls() {
        let model = test_model();
        let context = StreamContext {
            messages: vec![Message::Assistant {
                content: vec![
                    Content::Text {
                        text: "Let me help.".into(),
                    },
                    Content::ToolCall {
                        id: "call_123".into(),
                        name: "bash".into(),
                        arguments: serde_json::json!({"command": "ls"}),
                    },
                ],
                model: "gpt-4o".into(),
                usage: Usage::default(),
                stop_reason: StopReason::ToolUse,
                timestamp: now_ms(),
            }],
            tools: vec![],
        };
        let options = StreamOptions {
            api_key: "key".into(),
            system_prompt: None,
            max_tokens: None,
            thinking: ThinkingLevel::Off,
        };

        let body = build_request_body(&model, &context, &options, None);
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages[0]["role"], "assistant");
        assert_eq!(messages[0]["content"], "Let me help.");

        let tool_calls = messages[0]["tool_calls"].as_array().unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0]["id"], "call_123");
        assert_eq!(tool_calls[0]["function"]["name"], "bash");

        // Arguments should be a JSON string
        let args_str = tool_calls[0]["function"]["arguments"].as_str().unwrap();
        let args: Value = serde_json::from_str(args_str).unwrap();
        assert_eq!(args["command"], "ls");
    }

    #[test]
    fn tool_result_message() {
        let model = test_model();
        let context = StreamContext {
            messages: vec![Message::ToolResult {
                tool_call_id: "call_123".into(),
                tool_name: "bash".into(),
                content: vec![Content::Text {
                    text: "hello world".into(),
                }],
                is_error: false,
                timestamp: now_ms(),
            }],
            tools: vec![],
        };
        let options = StreamOptions {
            api_key: "key".into(),
            system_prompt: None,
            max_tokens: None,
            thinking: ThinkingLevel::Off,
        };

        let body = build_request_body(&model, &context, &options, None);
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages[0]["role"], "tool");
        assert_eq!(messages[0]["tool_call_id"], "call_123");
        assert_eq!(messages[0]["content"], "hello world");
    }

    #[test]
    fn tool_result_error_prefixed() {
        let model = test_model();
        let context = StreamContext {
            messages: vec![Message::ToolResult {
                tool_call_id: "call_456".into(),
                tool_name: "bash".into(),
                content: vec![Content::Text {
                    text: "command not found".into(),
                }],
                is_error: true,
                timestamp: now_ms(),
            }],
            tools: vec![],
        };
        let options = StreamOptions {
            api_key: "key".into(),
            system_prompt: None,
            max_tokens: None,
            thinking: ThinkingLevel::Off,
        };

        let body = build_request_body(&model, &context, &options, None);
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages[0]["content"], "Error: command not found");
    }

    #[test]
    fn tool_definitions_converted() {
        let model = test_model();
        let context = StreamContext {
            messages: vec![],
            tools: vec![ToolDef {
                name: "bash".into(),
                description: "Run shell commands".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": { "command": { "type": "string" } }
                }),
                deferred: false,
            }],
        };
        let options = StreamOptions {
            api_key: "key".into(),
            system_prompt: None,
            max_tokens: None,
            thinking: ThinkingLevel::Off,
        };

        let body = build_request_body(&model, &context, &options, None);
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["type"], "function");
        assert_eq!(tools[0]["function"]["name"], "bash");
        assert_eq!(tools[0]["function"]["description"], "Run shell commands");
    }

    #[test]
    fn server_tools_appended_to_tools_array() {
        let model = test_model();
        let context = StreamContext {
            messages: vec![Message::User {
                content: UserContent::Text("search".into()),
                timestamp: now_ms(),
            }],
            tools: vec![ToolDef {
                name: "read".into(),
                description: "Read file".into(),
                parameters: serde_json::json!({"type": "object"}),
                deferred: false,
            }],
        };
        let options = StreamOptions {
            api_key: "key".into(),
            system_prompt: None,
            max_tokens: None,
            thinking: ThinkingLevel::Off,
        };

        let server_tools = vec!["web_search".into(), "x_search".into()];
        let body = build_request_body(&model, &context, &options, Some(&server_tools));
        let tools = body["tools"].as_array().unwrap();
        // 1 local function tool + 2 server-side tools
        assert_eq!(tools.len(), 3);
        assert_eq!(tools[0]["type"], "function");
        assert_eq!(tools[1]["type"], "web_search");
        assert_eq!(tools[2]["type"], "x_search");
    }

    #[test]
    fn server_tools_only_no_local_tools() {
        let model = test_model();
        let context = StreamContext {
            messages: vec![Message::User {
                content: UserContent::Text("search".into()),
                timestamp: now_ms(),
            }],
            tools: vec![],
        };
        let options = StreamOptions {
            api_key: "key".into(),
            system_prompt: None,
            max_tokens: None,
            thinking: ThinkingLevel::Off,
        };

        let server_tools = vec!["web_search".into()];
        let body = build_request_body(&model, &context, &options, Some(&server_tools));
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["type"], "web_search");
        // No "function" wrapper
        assert!(tools[0].get("function").is_none());
    }
}
