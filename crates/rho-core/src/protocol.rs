//! Explicit conversions between the stable `rho-protocol` wire schema and
//! rho-core's provider-facing runtime types.
//!
//! The two representations deliberately are not treated as serde-compatible:
//! runtime messages contain timestamps and assistant response metadata that are
//! absent from the wire request, while the wire schema supports system messages
//! and remote images that rho-core cannot represent.

use rho_protocol as wire;
use thiserror::Error;

use crate::{
    models::ModelConfig,
    types::{Content, Message, Model, StopReason, Usage, UserContent},
};

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ProtocolConversionError {
    #[error("system messages must be supplied through RunRequest.system")]
    SystemMessage,
    #[error("message role {role} cannot contain a {block} block")]
    InvalidContentForRole {
        role: &'static str,
        block: &'static str,
    },
    #[error("tool messages must contain exactly one tool_result block")]
    InvalidToolMessage,
    #[error("URL images are not supported by the rho-core runtime")]
    UrlImageUnsupported,
    #[error("rho-core thinking blocks have no lossless rho-protocol representation")]
    ThinkingUnsupported,
    #[error("protocol usage cost_micros cannot be represented by rho-core Usage")]
    UsageCostUnsupported,
    #[error("model id mismatch: request has {requested:?}, config resolves {configured:?}")]
    ModelIdMismatch {
        requested: String,
        configured: String,
    },
    #[error("provider mismatch: request has {requested:?}, config resolves {configured:?}")]
    ProviderMismatch {
        requested: String,
        configured: String,
    },
}

/// Convert request messages while explicitly supplying the runtime timestamp.
pub fn messages_from_protocol(
    messages: &[wire::Message],
    timestamp: u64,
) -> Result<Vec<Message>, ProtocolConversionError> {
    messages
        .iter()
        .map(|message| message_from_protocol(message, timestamp))
        .collect()
}

pub fn message_from_protocol(
    message: &wire::Message,
    timestamp: u64,
) -> Result<Message, ProtocolConversionError> {
    match message.role {
        wire::MessageRole::System => Err(ProtocolConversionError::SystemMessage),
        wire::MessageRole::User => {
            let blocks = content_from_protocol(&message.content, "user", false)?;
            let content = match blocks.as_slice() {
                [Content::Text { text }] => UserContent::Text(text.clone()),
                _ => UserContent::Blocks(blocks),
            };
            Ok(Message::User { content, timestamp })
        }
        wire::MessageRole::Assistant => Ok(Message::Assistant {
            content: content_from_protocol(&message.content, "assistant", true)?,
            // These are populated once the provider responds. An assistant
            // message supplied as input has no corresponding wire metadata.
            model: String::new(),
            usage: Usage::default(),
            stop_reason: StopReason::Stop,
            timestamp,
        }),
        wire::MessageRole::Tool => {
            let [wire::ContentBlock::ToolResult(result)] = message.content.as_slice() else {
                return Err(ProtocolConversionError::InvalidToolMessage);
            };
            let content = match &result.content {
                None => Vec::new(),
                Some(serde_json::Value::String(text)) => vec![Content::Text { text: text.clone() }],
                Some(_) => {
                    return Err(ProtocolConversionError::InvalidContentForRole {
                        role: "tool",
                        block: "non-string tool_result content",
                    })
                }
            };
            Ok(Message::ToolResult {
                tool_call_id: result.call_id.clone(),
                tool_name: result.tool.clone(),
                content,
                is_error: !result.ok,
                timestamp,
            })
        }
    }
}

fn content_from_protocol(
    blocks: &[wire::ContentBlock],
    role: &'static str,
    allow_tool_calls: bool,
) -> Result<Vec<Content>, ProtocolConversionError> {
    blocks
        .iter()
        .map(|block| match block {
            wire::ContentBlock::Text { text } => Ok(Content::Text { text: text.clone() }),
            wire::ContentBlock::Image {
                source: wire::ImageSource::Base64 { media_type, data },
            } => Ok(Content::Image {
                data: data.clone(),
                mime_type: media_type.clone(),
            }),
            wire::ContentBlock::Image {
                source: wire::ImageSource::Url { .. },
            } => Err(ProtocolConversionError::UrlImageUnsupported),
            wire::ContentBlock::ToolCall(call) if allow_tool_calls => Ok(Content::ToolCall {
                id: call.call_id.clone(),
                name: call.tool.clone(),
                arguments: call.arguments.clone(),
            }),
            wire::ContentBlock::ToolCall(_) => {
                Err(ProtocolConversionError::InvalidContentForRole {
                    role,
                    block: "tool_call",
                })
            }
            wire::ContentBlock::ToolResult(_) => {
                Err(ProtocolConversionError::InvalidContentForRole {
                    role,
                    block: "tool_result",
                })
            }
        })
        .collect()
}

impl TryFrom<&Content> for wire::ContentBlock {
    type Error = ProtocolConversionError;

    fn try_from(content: &Content) -> Result<Self, Self::Error> {
        match content {
            Content::Text { text } => Ok(Self::Text { text: text.clone() }),
            Content::Image { data, mime_type } => Ok(Self::Image {
                source: wire::ImageSource::Base64 {
                    media_type: mime_type.clone(),
                    data: data.clone(),
                },
            }),
            Content::ToolCall {
                id,
                name,
                arguments,
            } => Ok(Self::ToolCall(wire::ToolCall {
                call_id: id.clone(),
                tool: name.clone(),
                arguments: arguments.clone(),
            })),
            Content::Thinking { .. } => Err(ProtocolConversionError::ThinkingUnsupported),
        }
    }
}

impl From<&Usage> for wire::Usage {
    fn from(usage: &Usage) -> Self {
        Self {
            input_tokens: usage.input,
            output_tokens: usage.output,
            cache_read_tokens: Some(usage.cache_read),
            cache_write_tokens: Some(usage.cache_write),
            cost_micros: None,
        }
    }
}

impl TryFrom<&wire::Usage> for Usage {
    type Error = ProtocolConversionError;

    fn try_from(usage: &wire::Usage) -> Result<Self, Self::Error> {
        if usage.cost_micros.is_some() {
            return Err(ProtocolConversionError::UsageCostUnsupported);
        }
        Ok(Self {
            input: usage.input_tokens,
            output: usage.output_tokens,
            cache_read: usage.cache_read_tokens.unwrap_or_default(),
            cache_write: usage.cache_write_tokens.unwrap_or_default(),
        })
    }
}

/// Resolve a wire model reference against a registry-selected config. Both the
/// public alias (`config.id`) and provider wire id (`config.model_id`) are
/// accepted, but provider identity must match exactly after normalization.
pub fn model_from_protocol(
    model: &wire::ModelRef,
    config: &ModelConfig,
) -> Result<Model, ProtocolConversionError> {
    let runtime = crate::models::ModelRegistry::to_model(config);
    if model.id != config.id && model.id != config.model_id {
        return Err(ProtocolConversionError::ModelIdMismatch {
            requested: model.id.clone(),
            configured: config.model_id.clone(),
        });
    }
    let configured_provider = protocol_provider(config, &runtime.provider);
    if normalize_provider(&model.provider) != configured_provider {
        return Err(ProtocolConversionError::ProviderMismatch {
            requested: model.provider.clone(),
            configured: configured_provider.to_owned(),
        });
    }
    Ok(runtime)
}

fn protocol_provider<'a>(config: &ModelConfig, runtime_provider: &'a str) -> &'a str {
    if config.base_url.trim_end_matches('/') == "https://api.x.ai/v1" {
        "xai"
    } else {
        normalize_provider(runtime_provider)
    }
}

fn normalize_provider(provider: &str) -> &str {
    match provider {
        "xai" | "xai-responses" => "xai",
        "openai-compatible" | "openai" => "openai",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ProviderType;
    use serde_json::json;

    #[test]
    fn converts_user_blocks_and_injects_timestamp() {
        let message = wire::Message {
            role: wire::MessageRole::User,
            content: vec![
                wire::ContentBlock::Text {
                    text: "look".into(),
                },
                wire::ContentBlock::Image {
                    source: wire::ImageSource::Base64 {
                        media_type: "image/png".into(),
                        data: "abc".into(),
                    },
                },
            ],
        };
        let Message::User {
            content: UserContent::Blocks(blocks),
            timestamp,
        } = message_from_protocol(&message, 42).unwrap()
        else {
            panic!("wrong message")
        };
        assert_eq!(timestamp, 42);
        assert_eq!(blocks.len(), 2);
    }

    #[test]
    fn rejects_unrepresentable_wire_content() {
        let system = wire::Message {
            role: wire::MessageRole::System,
            content: vec![],
        };
        assert!(matches!(
            message_from_protocol(&system, 0),
            Err(ProtocolConversionError::SystemMessage)
        ));

        let url = wire::Message {
            role: wire::MessageRole::User,
            content: vec![wire::ContentBlock::Image {
                source: wire::ImageSource::Url {
                    url: "https://example.test/a.png".into(),
                },
            }],
        };
        assert!(matches!(
            message_from_protocol(&url, 0),
            Err(ProtocolConversionError::UrlImageUnsupported)
        ));
    }

    #[test]
    fn converts_tool_messages_without_stringifying_json() {
        let tool = wire::Message {
            role: wire::MessageRole::Tool,
            content: vec![wire::ContentBlock::ToolResult(wire::ToolResult {
                call_id: "c1".into(),
                tool: "read".into(),
                ok: false,
                content: Some(json!("not found")),
            })],
        };
        let Message::ToolResult {
            tool_call_id,
            is_error,
            content,
            ..
        } = message_from_protocol(&tool, 7).unwrap()
        else {
            panic!("wrong message")
        };
        assert_eq!(tool_call_id, "c1");
        assert!(is_error);
        assert!(matches!(&content[0], Content::Text { text } if text == "not found"));
    }

    #[test]
    fn usage_conversion_rejects_untracked_cost() {
        let runtime = Usage {
            input: 1,
            output: 2,
            cache_read: 3,
            cache_write: 4,
        };
        let wire = wire::Usage::from(&runtime);
        assert_eq!(Usage::try_from(&wire).unwrap().cache_write, 4);
        let with_cost = wire::Usage {
            cost_micros: Some(9),
            ..wire
        };
        assert!(matches!(
            Usage::try_from(&with_cost),
            Err(ProtocolConversionError::UsageCostUnsupported)
        ));
    }

    #[test]
    fn resolved_model_must_match_provider_and_id() {
        let config = ModelConfig {
            id: "sonnet".into(),
            provider: ProviderType::Anthropic,
            model_id: "claude-sonnet-4-5".into(),
            base_url: String::new(),
            api_key_env: None,
            context_window: 200_000,
            max_tokens: 8_192,
            thinking: true,
            server_tools: None,
            llama_cpp: None,
        };
        let model = model_from_protocol(
            &wire::ModelRef {
                provider: "anthropic".into(),
                id: "sonnet".into(),
            },
            &config,
        )
        .unwrap();
        assert_eq!(model.id, "claude-sonnet-4-5");

        let error = model_from_protocol(
            &wire::ModelRef {
                provider: "openai".into(),
                id: "sonnet".into(),
            },
            &config,
        )
        .unwrap_err();
        assert!(matches!(
            error,
            ProtocolConversionError::ProviderMismatch { .. }
        ));
    }

    #[test]
    fn xai_openai_compatible_configs_have_xai_protocol_identity() {
        let config = ModelConfig {
            id: "grok".into(),
            provider: ProviderType::OpenAi,
            model_id: "grok-3".into(),
            base_url: "https://api.x.ai/v1".into(),
            api_key_env: Some("XAI_API_KEY".into()),
            context_window: 131_072,
            max_tokens: 8_192,
            thinking: false,
            server_tools: None,
            llama_cpp: None,
        };
        let model = model_from_protocol(
            &wire::ModelRef {
                provider: "xai".into(),
                id: "grok".into(),
            },
            &config,
        )
        .unwrap();
        assert_eq!(model.id, "grok-3");

        let mismatch = model_from_protocol(
            &wire::ModelRef {
                provider: "openai".into(),
                id: "grok".into(),
            },
            &config,
        )
        .unwrap_err();
        assert!(matches!(
            mismatch,
            ProtocolConversionError::ProviderMismatch { .. }
        ));
    }

    #[test]
    fn thinking_is_never_silently_dropped() {
        let result = wire::ContentBlock::try_from(&Content::Thinking {
            thinking: "secret".into(),
        });
        assert_eq!(result, Err(ProtocolConversionError::ThinkingUnsupported));
    }
}
