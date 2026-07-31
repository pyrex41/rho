use std::sync::Arc;

use crate::event_stream::{EventStream, EventStreamConsumer};
use crate::types::{AssistantStreamEvent, Message, Model, ThinkingLevel, ToolDef};

pub struct StreamOptions {
    pub api_key: String,
    pub system_prompt: Option<String>,
    pub max_tokens: Option<usize>,
    pub thinking: ThinkingLevel,
}

pub struct StreamContext {
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDef>,
}

pub type AssistantStream = EventStream<AssistantStreamEvent, Message>;

/// Type alias for the stream function signature used by agent_loop and providers.
/// Uses Arc to allow cloning (needed for loop mode which creates configs per iteration).
pub type StreamFn = Arc<
    dyn Fn(
            &Model,
            StreamContext,
            StreamOptions,
        ) -> EventStreamConsumer<AssistantStreamEvent, Message>
        + Send
        + Sync,
>;
