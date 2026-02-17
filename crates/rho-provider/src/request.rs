use rho_core::provider_types::{StreamContext, StreamOptions};
use rho_core::types::{Content, Message, Model, ThinkingLevel, ToolDef, UserContent};
use serde_json::{json, Value};

pub fn build_request_body(
    model: &Model,
    context: &StreamContext,
    options: &StreamOptions,
) -> Value {
    let mut body = json!({
        "model": model.id,
        "max_tokens": options.max_tokens.unwrap_or(model.max_tokens),
        "stream": true,
    });

    // Messages
    let messages = convert_messages(&context.messages);
    body["messages"] = Value::Array(messages);

    // System prompt
    if let Some(ref system) = options.system_prompt {
        body["system"] = json!(system);
    }

    // Tools
    if !context.tools.is_empty() {
        let tools: Vec<Value> = context.tools.iter().map(convert_tool_def).collect();
        body["tools"] = Value::Array(tools);
    }

    // Thinking
    if let Some(budget) = thinking_budget(options.thinking) {
        body["thinking"] = json!({
            "type": "enabled",
            "budget_tokens": budget,
        });
    }

    body
}

fn thinking_budget(level: ThinkingLevel) -> Option<usize> {
    match level {
        ThinkingLevel::Off => None,
        ThinkingLevel::Minimal => Some(1024),
        ThinkingLevel::Low => Some(2048),
        ThinkingLevel::Medium => Some(4096),
        ThinkingLevel::High => Some(8192),
    }
}

fn convert_messages(messages: &[Message]) -> Vec<Value> {
    let mut result = Vec::new();
    let mut i = 0;

    while i < messages.len() {
        match &messages[i] {
            Message::User { content, .. } => {
                result.push(convert_user_message(content));
                i += 1;
            }
            Message::Assistant { content, .. } => {
                result.push(convert_assistant_message(content));
                i += 1;
            }
            Message::ToolResult { .. } => {
                // Batch consecutive ToolResult messages into a single user message
                let mut tool_results = Vec::new();
                while i < messages.len() {
                    if let Message::ToolResult {
                        tool_call_id,
                        content,
                        is_error,
                        ..
                    } = &messages[i]
                    {
                        tool_results.push(convert_tool_result(tool_call_id, content, *is_error));
                        i += 1;
                    } else {
                        break;
                    }
                }
                result.push(json!({
                    "role": "user",
                    "content": tool_results,
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
            let converted: Vec<Value> = blocks.iter().map(convert_content_block).collect();
            json!({
                "role": "user",
                "content": converted,
            })
        }
    }
}

fn convert_assistant_message(content: &[Content]) -> Value {
    let blocks: Vec<Value> = content.iter().map(convert_content_block).collect();
    json!({
        "role": "assistant",
        "content": blocks,
    })
}

fn convert_content_block(content: &Content) -> Value {
    match content {
        Content::Text { text } => json!({
            "type": "text",
            "text": text,
        }),
        Content::Thinking { thinking } => json!({
            "type": "thinking",
            "thinking": thinking,
            "signature": "",
        }),
        Content::ToolCall {
            id,
            name,
            arguments,
        } => json!({
            "type": "tool_use",
            "id": id,
            "name": name,
            "input": arguments,
        }),
        Content::Image { data, mime_type } => json!({
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": mime_type,
                "data": data,
            },
        }),
    }
}

fn convert_tool_result(tool_call_id: &str, content: &[Content], is_error: bool) -> Value {
    // Extract text content for the tool result
    let text = content
        .iter()
        .filter_map(|c| match c {
            Content::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");

    let mut result = json!({
        "type": "tool_result",
        "tool_use_id": tool_call_id,
    });

    if !text.is_empty() {
        result["content"] = json!(text);
    }

    if is_error {
        result["is_error"] = json!(true);
    }

    result
}

fn convert_tool_def(tool: &ToolDef) -> Value {
    json!({
        "name": tool.name,
        "description": tool.description,
        "input_schema": tool.parameters,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rho_core::types::*;

    fn test_model() -> Model {
        Model {
            id: "claude-sonnet-4-5-20250929".into(),
            name: "Claude Sonnet".into(),
            provider: "anthropic".into(),
            base_url: String::new(),
            reasoning: false,
            context_window: 200000,
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

        assert_eq!(body["model"], "claude-sonnet-4-5-20250929");
        assert_eq!(body["max_tokens"], 8192);
        assert_eq!(body["stream"], true);
        assert_eq!(body["system"], "You are helpful.");
        assert!(body.get("thinking").is_none());
        assert!(body.get("tools").is_none());

        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"], "Hello");
    }

    #[test]
    fn test_thinking_budget() {
        let model = test_model();
        let context = StreamContext {
            messages: vec![Message::User {
                content: UserContent::Text("Think hard".into()),
                timestamp: 0,
            }],
            tools: vec![],
        };
        let options = StreamOptions {
            api_key: "test-key".into(),
            system_prompt: None,
            max_tokens: Some(4096),
            thinking: ThinkingLevel::High,
        };

        let body = build_request_body(&model, &context, &options);

        assert_eq!(body["max_tokens"], 4096);
        assert_eq!(body["thinking"]["type"], "enabled");
        assert_eq!(body["thinking"]["budget_tokens"], 8192);
        assert!(body.get("system").is_none());
    }

    #[test]
    fn test_tool_defs() {
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
        assert_eq!(tools[0]["name"], "read_file");
        assert_eq!(tools[0]["description"], "Read a file");
        assert!(tools[0]["input_schema"].is_object());
    }

    #[test]
    fn test_tool_result_batching() {
        let model = test_model();
        let context = StreamContext {
            messages: vec![
                Message::User {
                    content: UserContent::Text("Hi".into()),
                    timestamp: 0,
                },
                Message::Assistant {
                    content: vec![Content::ToolCall {
                        id: "call_1".into(),
                        name: "read".into(),
                        arguments: json!({}),
                    }],
                    model: "claude".into(),
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
                Message::ToolResult {
                    tool_call_id: "call_2".into(),
                    tool_name: "write".into(),
                    content: vec![Content::Text {
                        text: "error msg".into(),
                    }],
                    is_error: true,
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

        // User, Assistant, then batched ToolResults as one user message
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[1]["role"], "assistant");
        assert_eq!(messages[2]["role"], "user");

        let tool_results = messages[2]["content"].as_array().unwrap();
        assert_eq!(tool_results.len(), 2);
        assert_eq!(tool_results[0]["type"], "tool_result");
        assert_eq!(tool_results[0]["tool_use_id"], "call_1");
        assert_eq!(tool_results[0]["content"], "file contents");
        assert!(tool_results[0].get("is_error").is_none());

        assert_eq!(tool_results[1]["tool_use_id"], "call_2");
        assert_eq!(tool_results[1]["is_error"], true);
    }

    #[test]
    fn test_user_blocks() {
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
        assert_eq!(content[1]["type"], "image");
        assert_eq!(content[1]["source"]["type"], "base64");
        assert_eq!(content[1]["source"]["media_type"], "image/png");
    }

    #[test]
    fn test_assistant_content_conversion() {
        let model = test_model();
        let context = StreamContext {
            messages: vec![
                Message::User {
                    content: UserContent::Text("Hi".into()),
                    timestamp: 0,
                },
                Message::Assistant {
                    content: vec![
                        Content::Thinking {
                            thinking: "Let me think...".into(),
                        },
                        Content::Text {
                            text: "Hello!".into(),
                        },
                    ],
                    model: "claude".into(),
                    usage: Usage::default(),
                    stop_reason: StopReason::Stop,
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
        let assistant = &messages[1];
        let content = assistant["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "thinking");
        assert_eq!(content[0]["signature"], "");
        assert_eq!(content[1]["type"], "text");
    }
}
