pub mod error;
pub mod openai;
pub mod openai_request;
pub mod openai_response;
pub mod request;
pub mod response;
pub mod sse;

use futures::StreamExt;
use rho_core::event_stream::{EventStream, EventStreamProducer};
use rho_core::models::{ModelConfig, ProviderType};
use rho_core::provider_types::{AssistantStream, StreamContext, StreamFn, StreamOptions};
use rho_core::types::{AssistantStreamEvent, Message, Model, StopReason};

use crate::error::ProviderError;

/// Returns a closure that streams Anthropic API responses.
///
/// The returned closure takes a model, context, and options, then spawns
/// an async task to perform the HTTP request and SSE parsing. It returns
/// an EventStreamConsumer that yields AssistantStreamEvents as they arrive.
pub fn anthropic_stream_fn() -> StreamFn {
    std::sync::Arc::new(
        move |model: &Model, context: StreamContext, options: StreamOptions| {
            let model = model.clone();
            let stream: AssistantStream = EventStream::new();
            let (producer, consumer) = stream.split();

            tokio::spawn(async move {
                do_stream(model, context, options, producer).await;
            });

            consumer
        },
    )
}

async fn do_stream(
    model: Model,
    context: StreamContext,
    options: StreamOptions,
    mut producer: EventStreamProducer<AssistantStreamEvent, Message>,
) {
    match do_stream_inner(&model, &context, &options, &producer).await {
        Ok(msg) => {
            producer.end(Some(msg));
        }
        Err(e) => {
            tracing::error!("Provider stream error: {}", e);
            let _ = producer
                .push(AssistantStreamEvent::Error {
                    stop_reason: StopReason::Error,
                })
                .await;
            producer.end(None);
        }
    }
}

async fn do_stream_inner(
    model: &Model,
    context: &StreamContext,
    options: &StreamOptions,
    producer: &EventStreamProducer<AssistantStreamEvent, Message>,
) -> Result<Message, ProviderError> {
    let body = request::build_request_body(model, context, options);

    let client = reqwest::Client::new();
    let base_url = if model.base_url.is_empty() {
        "https://api.anthropic.com"
    } else {
        &model.base_url
    };

    let mut req = client
        .post(format!("{}/v1/messages", base_url))
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json");

    if rho_core::auth::is_oauth_token(&options.api_key) {
        req = req
            .header("Authorization", format!("Bearer {}", options.api_key))
            .header("anthropic-beta", "oauth-2025-04-20");
    } else {
        req = req.header("x-api-key", &options.api_key);
    }

    let resp = req.json(&body).send().await?;

    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        return Err(ProviderError::ApiError { status, body });
    }

    let byte_stream = resp.bytes_stream();
    let mut sse_stream = std::pin::pin!(sse::parse_sse_stream(byte_stream));
    let mut handler = response::ResponseHandler::new();

    while let Some(event_result) = sse_stream.next().await {
        match event_result {
            Ok(sse_event) => {
                let stream_events = handler.handle_event(&sse_event);
                for event in stream_events {
                    let _ = producer.push(event).await;
                }
            }
            Err(e) => {
                tracing::error!("SSE parse error: {}", e);
            }
        }
    }

    Ok(handler.build_final_message())
}

/// Select the appropriate `StreamFn` for a given model config.
///
/// Passes provider-managed server tools (e.g. xAI's `web_search`) through to the
/// OpenAI-compatible provider. Anthropic models ignore `server_tools`.
pub fn stream_fn_for_model(config: &ModelConfig) -> StreamFn {
    match config.provider {
        ProviderType::Anthropic => anthropic_stream_fn(),
        ProviderType::OpenAi => openai::openai_stream_fn(config.server_tools.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rho_core::models::ModelConfig;

    fn anthropic_config() -> ModelConfig {
        ModelConfig {
            id: "claude-sonnet".into(),
            provider: ProviderType::Anthropic,
            model_id: "claude-sonnet-4-5-20250929".into(),
            base_url: String::new(),
            api_key_env: Some("ANTHROPIC_API_KEY".into()),
            context_window: 200_000,
            max_tokens: 8_192,
            thinking: false,
            server_tools: None,
        }
    }

    fn openai_config() -> ModelConfig {
        ModelConfig {
            id: "grok-3".into(),
            provider: ProviderType::OpenAi,
            model_id: "grok-3".into(),
            base_url: "https://api.x.ai/v1".into(),
            api_key_env: Some("XAI_API_KEY".into()),
            context_window: 131_072,
            max_tokens: 16_384,
            thinking: false,
            server_tools: Some(vec!["web_search".into()]),
        }
    }

    #[test]
    fn test_anthropic_stream_fn_returns_closure() {
        let _f = anthropic_stream_fn();
    }

    #[test]
    fn test_openai_stream_fn_returns_closure() {
        let _f = openai::openai_stream_fn(None);
    }

    #[test]
    fn test_stream_fn_for_model_anthropic() {
        let _f = stream_fn_for_model(&anthropic_config());
    }

    #[test]
    fn test_stream_fn_for_model_openai_with_server_tools() {
        let _f = stream_fn_for_model(&openai_config());
    }
}
