use rho_core::provider_types::{StreamContext, StreamOptions};
use rho_core::types::{Content, Message, Model, ToolDef, UserContent};
use serde_json::{json, Value};

/// Build an OpenAI-compatible chat completions request body for xAI's API.
///
/// Key differences from Anthropic format:
/// - System prompt is a message with role "system" (not a top-level field)
/// - Tools use `{"type": "function", "function": {...}}` wrapper
/// - Tool results are individual messages with role "tool"
/// - Assistant tool calls use "tool_calls" array (not content blocks)
pub fn build_request_body(
    model: &Model,
    context: &StreamContext,
    options: &StreamOptions,
) -> Value {
    let mut body = json!({
        "model": model.id,
        "max_tokens": options.max_tokens.unwrap_or(model.max_tokens),
        "stream": true,
        "stream_options": { "include_usage": true },
    });

    // Build messages array
    let mut messages = Vec::new();

    // System prompt as first message
    if let Some(ref system) = options.system_prompt {
        messages.push(json!({
            "role": "system",
            "content": system,
        }));
    }

    // Convert conversation messages
    for msg in &context.messages {
        match msg {
            Message::User { content, .. } => {
                messages.push(convert_user_message(content));
            }
            Message::Assistant { content, .. } => {
                messages.push(convert_assistant_message(content));
            }
            Message::ToolResult {
                tool_call_id,
                content,
                ..
            } => {
                messages.push(convert_tool_result(tool_call_id, content));
            }
        }
    }

    body["messages"] = Value::Array(messages);

    // Tools (function calling)
    if !context.tools.is_empty() {
        let tools: Vec<Value> = context.tools.iter().map(convert_tool_def).collect();
        body["tools"] = Value::Array(tools);
    }

    // Add xAI server-side tools if configured on the model
    // These are passed as additional tool entries alongside function tools.
    // The model's base_url field is used for routing; server-side tools
    // are configured via the xai_tools field on the Model.
    if let Some(xai_tools) = &model.xai_tools {
        let tools = body
            .get_mut("tools")
            .and_then(|v| v.as_array_mut());
        let tools_arr = match tools {
            Some(arr) => arr,
            None => {
                body["tools"] = json!([]);
                body["tools"].as_array_mut().unwrap()
            }
        };
        for tool_name in xai_tools {
            match tool_name.as_str() {
                "web_search" => {
                    tools_arr.push(json!({"type": "web_search"}));
                }
                "x_search" => {
                    tools_arr.push(json!({"type": "x_search"}));
                }
                _ => {
                    tracing::warn!("Unknown xAI server-side tool: {}", tool_name);
                }
            }
        }
    }

    body
}

fn convert_user_message(content: &UserContent) -> Value {
    match content {
        UserContent::Text(s) => json!({
            "role": "user",
            "content": s,
        }),
        UserContent::Blocks(blocks) => {
            let converted: Vec<Value> = blocks.iter().map(convert_content_to_openai).collect();
            json!({
                "role": "user",
                "content": converted,
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
                text_parts.push(text.clone());
            }
            Content::Thinking { .. } => {
                // xAI doesn't use Anthropic-style thinking blocks.
                // Skip thinking content in outgoing messages.
            }
            Content::ToolCall {
                id,
                name,
                arguments,
            } => {
                // Arguments must be a JSON string in OpenAI format
                let args_str = match arguments {
                    Value::String(s) => s.clone(),
                    other => serde_json::to_string(other).unwrap_or_default(),
                };
                tool_calls.push(json!({
                    "id": id,
                    "type": "function",
                    "function": {
                        "name": name,
                        "arguments": args_str,
                    }
                }));
            }
            Content::Image { .. } => {
                // Images in assistant messages are not standard in OpenAI format
            }
        }
    }

    let mut msg = json!({ "role": "assistant" });

    let combined_text = text_parts.join("");
    if !combined_text.is_empty() {
        msg["content"] = json!(combined_text);
    } else if tool_calls.is_empty() {
        // Must have some content
        msg["content"] = json!("");
    }

    if !tool_calls.is_empty() {
        msg["tool_calls"] = Value::Array(tool_calls);
    }

    msg
}

fn convert_content_to_openai(content: &Content) -> Value {
    match content {
        Content::Text { text } => json!({
            "type": "text",
            "text": text,
        }),
        Content::Image { data, mime_type } => json!({
            "type": "image_url",
            "image_url": {
                "url": format!("data:{};base64,{}", mime_type, data),
            }
        }),
        Content::Thinking { .. } => json!({
            "type": "text",
            "text": "",
        }),
        Content::ToolCall { .. } => {
            // Tool calls shouldn't appear in user content blocks
            json!({"type": "text", "text": ""})
        }
    }
}

fn convert_tool_result(tool_call_id: &str, content: &[Content]) -> Value {
    let text = content
        .iter()
        .filter_map(|c| match c {
            Content::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");

    json!({
        "role": "tool",
        "tool_call_id": tool_call_id,
        "content": text,
    })
}

fn convert_tool_def(tool: &ToolDef) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": tool.name,
            "description": tool.description,
            "parameters": tool.parameters,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rho_core::types::*;

    fn test_model() -> Model {
        Model {
            id: "grok-3".into(),
            name: "Grok 3".into(),
            provider: "xai".into(),
            base_url: String::new(),
            reasoning: false,
            context_window: 131_072,
            max_tokens: 8192,
            xai_tools: None,
        }
    }

    #[test]
    fn test_basic_request() {
        let model = test_model();
        let context = StreamContext {
            messages: vec![Message::User {
                content: UserContent::Text("Hello".into()),
                timestamp: 0,
            }],
            tools: vec![],
        };
        let options = StreamOptions {
            api_key: "test-key".into(),
            system_prompt: Some("You are helpful.".into()),
            max_tokens: None,
            thinking: ThinkingLevel::Off,
        };

        let body = build_request_body(&model, &context, &options);

        assert_eq!(body["model"], "grok-3");
        assert_eq!(body["max_tokens"], 8192);
        assert_eq!(body["stream"], true);
        assert!(body.get("tools").is_none());

        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2); // system + user
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "You are helpful.");
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[1]["content"], "Hello");
    }

    #[test]
    fn test_tool_defs_openai_format() {
        let model = test_model();
        let context = StreamContext {
            messages: vec![Message::User {
                content: UserContent::Text("Use a tool".into()),
                timestamp: 0,
            }],
            tools: vec![ToolDef {
                name: "read_file".into(),
                description: "Read a file".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" }
                    },
                    "required": ["path"]
                }),
            }],
        };
        let options = StreamOptions {
            api_key: "key".into(),
            system_prompt: None,
            max_tokens: None,
            thinking: ThinkingLevel::Off,
        };

        let body = build_request_body(&model, &context, &options);
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["type"], "function");
        assert_eq!(tools[0]["function"]["name"], "read_file");
        assert_eq!(tools[0]["function"]["description"], "Read a file");
        assert!(tools[0]["function"]["parameters"].is_object());
    }

    #[test]
    fn test_assistant_with_tool_calls() {
        let model = test_model();
        let context = StreamContext {
            messages: vec![
                Message::User {
                    content: UserContent::Text("Hi".into()),
                    timestamp: 0,
                },
                Message::Assistant {
                    content: vec![
                        Content::Text {
                            text: "Let me read that file.".into(),
                        },
                        Content::ToolCall {
                            id: "call_1".into(),
                            name: "read".into(),
                            arguments: json!({"path": "/tmp/test.txt"}),
                        },
                    ],
                    model: "grok-3".into(),
                    usage: Usage::default(),
                    stop_reason: StopReason::ToolUse,
                    timestamp: 0,
                },
                Message::ToolResult {
                    tool_call_id: "call_1".into(),
                    tool_name: "read".into(),
                    content: vec![Content::Text {
                        text: "file contents".into(),
                    }],
                    is_error: false,
                    timestamp: 0,
                },
            ],
            tools: vec![],
        };
        let options = StreamOptions {
            api_key: "key".into(),
            system_prompt: None,
            max_tokens: None,
            thinking: ThinkingLevel::Off,
        };

        let body = build_request_body(&model, &context, &options);
        let messages = body["messages"].as_array().unwrap();

        // User, Assistant (with tool_calls), Tool result
        assert_eq!(messages.len(), 3);

        // Assistant message should have content + tool_calls
        assert_eq!(messages[1]["role"], "assistant");
        assert_eq!(messages[1]["content"], "Let me read that file.");
        let tool_calls = messages[1]["tool_calls"].as_array().unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0]["id"], "call_1");
        assert_eq!(tool_calls[0]["type"], "function");
        assert_eq!(tool_calls[0]["function"]["name"], "read");

        // Tool result
        assert_eq!(messages[2]["role"], "tool");
        assert_eq!(messages[2]["tool_call_id"], "call_1");
        assert_eq!(messages[2]["content"], "file contents");
    }

    #[test]
    fn test_xai_server_side_tools() {
        let mut model = test_model();
        model.xai_tools = Some(vec!["web_search".into(), "x_search".into()]);

        let context = StreamContext {
            messages: vec![Message::User {
                content: UserContent::Text("Search the web".into()),
                timestamp: 0,
            }],
            tools: vec![ToolDef {
                name: "read_file".into(),
                description: "Read a file".into(),
                parameters: json!({"type": "object"}),
            }],
        };
        let options = StreamOptions {
            api_key: "key".into(),
            system_prompt: None,
            max_tokens: None,
            thinking: ThinkingLevel::Off,
        };

        let body = build_request_body(&model, &context, &options);
        let tools = body["tools"].as_array().unwrap();
        // 1 function tool + 2 server-side tools
        assert_eq!(tools.len(), 3);
        assert_eq!(tools[0]["type"], "function");
        assert_eq!(tools[1]["type"], "web_search");
        assert_eq!(tools[2]["type"], "x_search");
    }

    #[test]
    fn test_image_in_user_message() {
        let model = test_model();
        let context = StreamContext {
            messages: vec![Message::User {
                content: UserContent::Blocks(vec![
                    Content::Text {
                        text: "Look at this".into(),
                    },
                    Content::Image {
                        data: "abc123".into(),
                        mime_type: "image/png".into(),
                    },
                ]),
                timestamp: 0,
            }],
            tools: vec![],
        };
        let options = StreamOptions {
            api_key: "key".into(),
            system_prompt: None,
            max_tokens: None,
            thinking: ThinkingLevel::Off,
        };

        let body = build_request_body(&model, &context, &options);
        let messages = body["messages"].as_array().unwrap();
        let content = messages[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "image_url");
        assert!(content[1]["image_url"]["url"]
            .as_str()
            .unwrap()
            .starts_with("data:image/png;base64,"));
    }
}
