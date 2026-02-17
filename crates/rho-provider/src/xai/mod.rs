pub mod request;
pub mod response;

use futures::StreamExt;
use rho_core::event_stream::{EventStream, EventStreamProducer};
use rho_core::provider_types::{AssistantStream, StreamContext, StreamFn, StreamOptions};
use rho_core::types::{AssistantStreamEvent, Message, Model, StopReason};

use crate::error::ProviderError;

/// Returns a closure that streams xAI API responses using the OpenAI-compatible
/// chat completions endpoint (`/v1/chat/completions`).
///
/// The xAI API is OpenAI-compatible, so this uses the standard OpenAI SSE
/// streaming format with `chat.completion.chunk` objects.
pub fn xai_stream_fn() -> StreamFn {
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
            tracing::error!("xAI provider stream error: {}", e);
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
        "https://api.x.ai"
    } else {
        &model.base_url
    };

    let resp = client
        .post(format!("{}/v1/chat/completions", base_url))
        .header("content-type", "application/json")
        .header("Authorization", format!("Bearer {}", options.api_key))
        .json(&body)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        return Err(ProviderError::ApiError { status, body });
    }

    let byte_stream = resp.bytes_stream();
    let mut sse_stream = std::pin::pin!(crate::sse::parse_sse_stream(byte_stream));
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
                tracing::error!("xAI SSE parse error: {}", e);
            }
        }
    }

    Ok(handler.build_final_message())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_xai_stream_fn_returns_closure() {
        let _f = xai_stream_fn();
    }
}
