use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, Sse};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use futures::stream::Stream;
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, RwLock};
use tokio_util::sync::CancellationToken;

use rho_core::agent_loop::{agent_loop, AgentLoopConfig};
use rho_core::compaction;
use rho_core::config::ProjectConfig;
use rho_core::models::{ModelConfig, ModelRegistry};
use rho_core::tool::AgentTool;
use rho_core::types::*;

// === Server State ===

pub struct ServerState {
    sessions: RwLock<HashMap<String, SessionHandle>>,
    config: ProjectConfig,
    registry: ModelRegistry,
    default_model_config: ModelConfig,
    api_key: String,
    #[allow(dead_code)]
    cwd: PathBuf,
    tools: Vec<Arc<dyn AgentTool>>,
    system_prompt: String,
    #[allow(dead_code)]
    auth_token: Option<String>,
}

struct SessionHandle {
    id: String,
    messages: Vec<Message>,
    model_config: ModelConfig,
    event_tx: broadcast::Sender<ServerEvent>,
    cancel: CancellationToken,
    created_at: u64,
    busy: bool,
}

// === API Types ===

#[derive(Debug, Deserialize)]
pub struct CreateSessionRequest {
    pub model: Option<String>,
    pub thinking: Option<String>,
    pub cwd: Option<String>,
    pub system_append: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CreateSessionResponse {
    pub id: String,
    pub model: String,
    pub created_at: u64,
}

#[derive(Debug, Deserialize)]
pub struct SendMessageRequest {
    pub content: String,
}

#[derive(Debug, Serialize)]
pub struct SendMessageResponse {
    pub accepted: bool,
    pub turn_index: usize,
}

#[derive(Debug, Serialize)]
pub struct SessionInfoResponse {
    pub id: String,
    pub model: String,
    pub message_count: usize,
    pub created_at: u64,
    pub busy: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum ServerEvent {
    #[serde(rename = "turn_start")]
    TurnStart,
    #[serde(rename = "text_delta")]
    TextDelta { delta: String },
    #[serde(rename = "thinking_delta")]
    ThinkingDelta { delta: String },
    #[serde(rename = "tool_start")]
    ToolStart {
        tool_name: String,
        args: serde_json::Value,
    },
    #[serde(rename = "tool_end")]
    ToolEnd { tool_name: String, is_error: bool },
    #[serde(rename = "turn_end")]
    TurnEnd { usage: Option<Usage> },
    #[serde(rename = "context_compacted")]
    ContextCompacted {
        original_estimate: usize,
        compacted_estimate: usize,
    },
    #[serde(rename = "agent_end")]
    AgentEnd,
    #[serde(rename = "error")]
    Error { message: String },
}

// === Server Builder ===

pub struct ServerBuilder {
    config: ProjectConfig,
    registry: ModelRegistry,
    default_model_config: ModelConfig,
    api_key: String,
    cwd: PathBuf,
    tools: Vec<Arc<dyn AgentTool>>,
    system_prompt: String,
    auth_token: Option<String>,
}

impl ServerBuilder {
    pub fn new(
        config: ProjectConfig,
        registry: ModelRegistry,
        default_model_config: ModelConfig,
        api_key: String,
        cwd: PathBuf,
        tools: Vec<Arc<dyn AgentTool>>,
        system_prompt: String,
    ) -> Self {
        Self {
            config,
            registry,
            default_model_config,
            api_key,
            cwd,
            tools,
            system_prompt,
            auth_token: None,
        }
    }

    pub fn with_auth_token(mut self, token: String) -> Self {
        self.auth_token = Some(token);
        self
    }

    pub fn build(self) -> Router {
        let state = Arc::new(ServerState {
            sessions: RwLock::new(HashMap::new()),
            config: self.config,
            registry: self.registry,
            default_model_config: self.default_model_config,
            api_key: self.api_key,
            cwd: self.cwd,
            tools: self.tools,
            system_prompt: self.system_prompt,
            auth_token: self.auth_token,
        });

        Router::new()
            .route("/health", get(health))
            .route("/v1/sessions", post(create_session))
            .route("/v1/sessions", get(list_sessions))
            .route("/v1/sessions/{id}", get(get_session))
            .route("/v1/sessions/{id}", delete(delete_session))
            .route("/v1/sessions/{id}/send", post(send_message))
            .route("/v1/sessions/{id}/events", get(session_events))
            .with_state(state)
    }
}

// === Route Handlers ===

async fn health() -> &'static str {
    "ok"
}

async fn create_session(
    State(state): State<Arc<ServerState>>,
    Json(req): Json<CreateSessionRequest>,
) -> Result<Json<CreateSessionResponse>, StatusCode> {
    let session_id = uuid::Uuid::new_v4().to_string();
    let now = now_ms();

    let model_config = if let Some(ref model_id) = req.model {
        state
            .registry
            .get(model_id)
            .cloned()
            .unwrap_or_else(|| state.default_model_config.clone())
    } else {
        state.default_model_config.clone()
    };

    let (event_tx, _) = broadcast::channel(256);
    let cancel = CancellationToken::new();

    let handle = SessionHandle {
        id: session_id.clone(),
        messages: Vec::new(),
        model_config: model_config.clone(),
        event_tx,
        cancel,
        created_at: now,
        busy: false,
    };

    let model_id = model_config.model_id.clone();
    state.sessions.write().await.insert(session_id.clone(), handle);

    Ok(Json(CreateSessionResponse {
        id: session_id,
        model: model_id,
        created_at: now,
    }))
}

async fn list_sessions(
    State(state): State<Arc<ServerState>>,
) -> Json<Vec<SessionInfoResponse>> {
    let sessions = state.sessions.read().await;
    let list: Vec<SessionInfoResponse> = sessions
        .values()
        .map(|s| SessionInfoResponse {
            id: s.id.clone(),
            model: s.model_config.model_id.clone(),
            message_count: s.messages.len(),
            created_at: s.created_at,
            busy: s.busy,
        })
        .collect();
    Json(list)
}

async fn get_session(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
) -> Result<Json<SessionInfoResponse>, StatusCode> {
    let sessions = state.sessions.read().await;
    let handle = sessions.get(&id).ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(SessionInfoResponse {
        id: handle.id.clone(),
        model: handle.model_config.model_id.clone(),
        message_count: handle.messages.len(),
        created_at: handle.created_at,
        busy: handle.busy,
    }))
}

async fn delete_session(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
) -> Result<StatusCode, StatusCode> {
    let mut sessions = state.sessions.write().await;
    let handle = sessions.remove(&id).ok_or(StatusCode::NOT_FOUND)?;
    handle.cancel.cancel();
    Ok(StatusCode::NO_CONTENT)
}

async fn send_message(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
    Json(req): Json<SendMessageRequest>,
) -> Result<Json<SendMessageResponse>, (StatusCode, String)> {
    // Get session info under read lock, then drop it
    let (messages, model_config, event_tx, cancel, turn_index) = {
        let mut sessions = state.sessions.write().await;
        let handle = sessions
            .get_mut(&id)
            .ok_or((StatusCode::NOT_FOUND, "Session not found".to_string()))?;

        if handle.busy {
            return Err((
                StatusCode::CONFLICT,
                "Session is busy processing a message".to_string(),
            ));
        }

        handle.messages.push(Message::User {
            content: UserContent::Text(req.content),
            timestamp: now_ms(),
        });
        handle.busy = true;

        let turn_index = handle
            .messages
            .iter()
            .filter(|m| matches!(m, Message::User { .. }))
            .count()
            - 1;

        (
            handle.messages.clone(),
            handle.model_config.clone(),
            handle.event_tx.clone(),
            handle.cancel.clone(),
            turn_index,
        )
    };

    // Spawn the agent loop in a background task
    let state_clone = state.clone();
    let id_clone = id.clone();

    tokio::spawn(async move {
        run_agent_for_session(
            state_clone,
            id_clone,
            messages,
            model_config,
            event_tx,
            cancel,
        )
        .await;
    });

    Ok(Json(SendMessageResponse {
        accepted: true,
        turn_index,
    }))
}

async fn run_agent_for_session(
    state: Arc<ServerState>,
    session_id: String,
    messages: Vec<Message>,
    model_config: ModelConfig,
    event_tx: broadcast::Sender<ServerEvent>,
    cancel: CancellationToken,
) {
    let model = ModelRegistry::to_model(&model_config);
    let thinking = state.config.thinking.unwrap_or(ThinkingLevel::Off);

    let transform_messages = state
        .config
        .compact_threshold
        .map(compaction::make_compaction_transform);

    let config = AgentLoopConfig {
        model,
        api_key: state.api_key.clone(),
        system_prompt: state.system_prompt.clone(),
        tools: state.tools.clone(),
        thinking,
        max_tokens: None,
        stream_fn: rho_provider::stream_fn_for_model(&model_config),
        get_steering_messages: None,
        get_follow_up_messages: None,
        transform_messages,
    };

    let mut consumer = agent_loop(messages, config, cancel);

    while let Some(event) = consumer.next().await {
        let server_event = match &event {
            AgentEvent::TurnStart => Some(ServerEvent::TurnStart),
            AgentEvent::MessageUpdate { event, .. } => match event {
                AssistantStreamEvent::TextDelta { delta, .. } => {
                    Some(ServerEvent::TextDelta {
                        delta: delta.clone(),
                    })
                }
                AssistantStreamEvent::ThinkingDelta { delta, .. } => {
                    Some(ServerEvent::ThinkingDelta {
                        delta: delta.clone(),
                    })
                }
                _ => None,
            },
            AgentEvent::ToolExecutionStart {
                tool_name, args, ..
            } => Some(ServerEvent::ToolStart {
                tool_name: tool_name.clone(),
                args: args.clone(),
            }),
            AgentEvent::ToolExecutionEnd {
                tool_name,
                is_error,
                ..
            } => Some(ServerEvent::ToolEnd {
                tool_name: tool_name.clone(),
                is_error: *is_error,
            }),
            AgentEvent::TurnEnd { .. } => Some(ServerEvent::TurnEnd { usage: None }),
            AgentEvent::ContextCompacted {
                original_estimate,
                compacted_estimate,
                ..
            } => Some(ServerEvent::ContextCompacted {
                original_estimate: *original_estimate,
                compacted_estimate: *compacted_estimate,
            }),
            AgentEvent::AgentEnd { messages } => {
                // Update session with final messages
                let mut sessions = state.sessions.write().await;
                if let Some(handle) = sessions.get_mut(&session_id) {
                    handle.messages = messages.clone();
                    handle.busy = false;
                }
                Some(ServerEvent::AgentEnd)
            }
            _ => None,
        };

        if let Some(se) = server_event {
            let _ = event_tx.send(se);
        }
    }

    // Ensure busy is cleared even on error
    let mut sessions = state.sessions.write().await;
    if let Some(handle) = sessions.get_mut(&session_id) {
        handle.busy = false;
    }
}

async fn session_events(
    State(state): State<Arc<ServerState>>,
    Path(id): Path<String>,
) -> Result<Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>>, StatusCode> {
    let sessions = state.sessions.read().await;
    let handle = sessions.get(&id).ok_or(StatusCode::NOT_FOUND)?;
    let mut rx = handle.event_tx.subscribe();
    drop(sessions);

    let stream = async_stream::stream! {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    if let Ok(data) = serde_json::to_string(&event) {
                        yield Ok(Event::default().data(data));
                    }
                    if matches!(event, ServerEvent::AgentEnd) {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Closed) => break,
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    let msg = format!("{{\"type\":\"error\",\"message\":\"missed {} events\"}}", n);
                    yield Ok(Event::default().data(msg));
                }
            }
        }
    };

    Ok(Sse::new(stream))
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

    #[test]
    fn server_event_serialization() {
        let event = ServerEvent::TextDelta {
            delta: "hello".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"type\":\"text_delta\""));
        assert!(json.contains("\"delta\":\"hello\""));
    }

    #[test]
    fn server_event_tool_start_serialization() {
        let event = ServerEvent::ToolStart {
            tool_name: "read".to_string(),
            args: serde_json::json!({"path": "/tmp/test.rs"}),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"type\":\"tool_start\""));
        assert!(json.contains("\"tool_name\":\"read\""));
    }

    #[test]
    fn server_event_agent_end() {
        let event = ServerEvent::AgentEnd;
        let json = serde_json::to_string(&event).unwrap();
        assert_eq!(json, "{\"type\":\"agent_end\"}");
    }
}
