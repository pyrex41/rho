pub mod error;
pub mod request;
pub mod response;
pub mod sse;

use futures::StreamExt;
use rho_core::event_stream::{EventStream, EventStreamProducer};
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

    if anthropic_auth::is_oauth_token(&options.api_key) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_anthropic_stream_fn_returns_closure() {
        // Compile-time check that the closure has the right signature
        let _f = anthropic_stream_fn();
    }
}
