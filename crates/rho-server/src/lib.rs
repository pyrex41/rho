use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Json,
    },
    routing::{delete, get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use rho_core::agent_loop::{agent_loop, AgentLoopConfig};
use rho_core::compaction;
use rho_core::models::{ModelConfig, ModelRegistry};
use rho_core::tool::AgentTool;
use rho_core::types::*;

// === Config & State ===

#[derive(Clone)]
pub struct ServerConfig {
    pub model_config: ModelConfig,
    pub api_key: String,
    pub system_prompt: String,
    pub tools_factory: Arc<dyn Fn() -> Vec<Arc<dyn AgentTool>> + Send + Sync>,
    pub thinking: ThinkingLevel,
    pub bearer_token: Option<String>,
    pub compact_threshold: Option<f64>,
    pub cwd: PathBuf,
}

struct SessionHandle {
    messages: Vec<Message>,
    model_id: String,
    cancel: CancellationToken,
    created_at: u64,
}

struct AppState {
    sessions: RwLock<HashMap<String, SessionHandle>>,
    config: ServerConfig,
}

fn check_bearer_token(state: &AppState, headers: &HeaderMap) -> Result<(), StatusCode> {
    if let Some(ref expected) = state.config.bearer_token {
        let auth = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "));
        match auth {
            Some(token) if token == expected => Ok(()),
            _ => Err(StatusCode::UNAUTHORIZED),
        }
    } else {
        Ok(())
    }
}

/// Helper macro to check auth at the start of each handler.
/// Health endpoint is unauthenticated.
macro_rules! require_auth {
    ($state:expr, $headers:expr) => {
        if let Err(status) = check_bearer_token(&$state, &$headers) {
            return Err(status);
        }
    };
}

// === Route handlers ===

async fn health() -> &'static str {
    "ok"
}

#[derive(Deserialize)]
struct CreateSessionRequest {
    model: Option<String>,
}

#[derive(Serialize)]
struct SessionInfo {
    id: String,
    model: String,
    message_count: usize,
    created_at: u64,
}

async fn create_session(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<CreateSessionRequest>,
) -> Result<Json<SessionInfo>, StatusCode> {
    require_auth!(state, headers);

    let id = uuid::Uuid::new_v4().to_string();
    let model_id = body
        .model
        .unwrap_or_else(|| state.config.model_config.id.clone());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;

    let handle = SessionHandle {
        messages: Vec::new(),
        model_id: model_id.clone(),
        cancel: CancellationToken::new(),
        created_at: now,
    };

    let info = SessionInfo {
        id: id.clone(),
        model: model_id,
        message_count: 0,
        created_at: now,
    };

    state.sessions.write().await.insert(id, handle);
    Ok(Json(info))
}

async fn list_sessions(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Vec<SessionInfo>>, StatusCode> {
    require_auth!(state, headers);

    let sessions = state.sessions.read().await;
    let list: Vec<SessionInfo> = sessions
        .iter()
        .map(|(id, h)| SessionInfo {
            id: id.clone(),
            model: h.model_id.clone(),
            message_count: h.messages.len(),
            created_at: h.created_at,
        })
        .collect();
    Ok(Json(list))
}

async fn get_session(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<SessionInfo>, StatusCode> {
    require_auth!(state, headers);

    let sessions = state.sessions.read().await;
    let handle = sessions.get(&id).ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(SessionInfo {
        id: id.clone(),
        model: handle.model_id.clone(),
        message_count: handle.messages.len(),
        created_at: handle.created_at,
    }))
}

async fn delete_session_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode, StatusCode> {
    require_auth!(state, headers);

    let mut sessions = state.sessions.write().await;
    if let Some(handle) = sessions.remove(&id) {
        handle.cancel.cancel();
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}

#[derive(Deserialize)]
struct SendMessageRequest {
    message: String,
}

async fn send_message(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<SendMessageRequest>,
) -> Result<impl IntoResponse, StatusCode> {
    require_auth!(state, headers);

    let (messages, cancel) = {
        let mut sessions = state.sessions.write().await;
        let handle = sessions.get_mut(&id).ok_or(StatusCode::NOT_FOUND)?;

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        handle.messages.push(Message::User {
            content: UserContent::Text(body.message),
            timestamp: now,
        });

        let cancel = CancellationToken::new();
        handle.cancel = cancel.clone();
        (handle.messages.clone(), cancel)
    };

    let config = &state.config;
    let model = ModelRegistry::to_model(&config.model_config);
    let tools = (config.tools_factory)();
    let transform_messages = config
        .compact_threshold
        .map(|t| compaction::make_compaction_transform(t, None));

    let loop_config = AgentLoopConfig {
        model,
        api_key: config.api_key.clone(),
        system_prompt: config.system_prompt.clone(),
        tools,
        thinking: config.thinking,
        max_tokens: None,
        stream_fn: rho_provider::stream_fn_for_model(&config.model_config),
        get_steering_messages: None,
        get_follow_up_messages: None,
        transform_messages,
        post_tools_hooks: Vec::new(),
        pre_tool_hooks: vec![],
        lifecycle_hooks: vec![],
        shared_messages: None,
    };

    let mut consumer = agent_loop(messages, loop_config, cancel);

    let state_clone = state.clone();
    let session_id = id.clone();

    let stream = async_stream::stream! {
        while let Some(event) = consumer.next().await {
            let sse = match &event {
                AgentEvent::MessageUpdate { event: stream_event, .. } => match stream_event {
                    AssistantStreamEvent::TextDelta { delta, .. } => {
                        Some(Event::default()
                            .event("text_delta")
                            .json_data(serde_json::json!({"text": delta})))
                    }
                    _ => None,
                },
                AgentEvent::ToolExecutionStart { tool_call_id, tool_name, args } => {
                    Some(Event::default()
                        .event("tool_start")
                        .json_data(serde_json::json!({
                            "tool_id": tool_call_id,
                            "tool_name": tool_name,
                            "args": args
                        })))
                }
                AgentEvent::ToolExecutionEnd { tool_call_id, tool_name, is_error, .. } => {
                    Some(Event::default()
                        .event("tool_end")
                        .json_data(serde_json::json!({
                            "tool_id": tool_call_id,
                            "tool_name": tool_name,
                            "is_error": is_error
                        })))
                }
                AgentEvent::TurnStart => {
                    Some(Event::default()
                        .event("turn_start")
                        .json_data(serde_json::json!({})))
                }
                AgentEvent::TurnEnd { .. } => {
                    Some(Event::default()
                        .event("turn_end")
                        .json_data(serde_json::json!({})))
                }
                AgentEvent::ContextCompacted { original_estimate, compacted_estimate, .. } => {
                    Some(Event::default()
                        .event("context_compacted")
                        .json_data(serde_json::json!({
                            "original_estimate": original_estimate,
                            "compacted_estimate": compacted_estimate
                        })))
                }
                AgentEvent::AgentEnd { messages } => {
                    // Store final messages back into session
                    let mut sessions = state_clone.sessions.write().await;
                    if let Some(handle) = sessions.get_mut(&session_id) {
                        handle.messages = messages.clone();
                    }
                    Some(Event::default()
                        .event("done")
                        .json_data(serde_json::json!({"message_count": messages.len()})))
                }
                _ => None,
            };

            if let Some(Ok(e)) = sse {
                yield Ok::<_, std::convert::Infallible>(e);
            }
        }
    };

    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

async fn get_events(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Vec<serde_json::Value>>, StatusCode> {
    require_auth!(state, headers);

    let sessions = state.sessions.read().await;
    let handle = sessions.get(&id).ok_or(StatusCode::NOT_FOUND)?;

    let events: Vec<serde_json::Value> = handle
        .messages
        .iter()
        .map(|msg| serde_json::to_value(msg).unwrap_or_default())
        .collect();

    Ok(Json(events))
}

// === Server entry point ===

pub async fn start_server_with_addr(
    config: ServerConfig,
    host: &str,
    port: u16,
) -> anyhow::Result<()> {
    let state = Arc::new(AppState {
        sessions: RwLock::new(HashMap::new()),
        config,
    });

    let app = Router::new()
        .route("/health", get(health))
        .route("/v1/sessions", post(create_session))
        .route("/v1/sessions", get(list_sessions))
        .route("/v1/sessions/{id}", get(get_session))
        .route("/v1/sessions/{id}", delete(delete_session_handler))
        .route("/v1/sessions/{id}/send", post(send_message))
        .route("/v1/sessions/{id}/events", get(get_events))
        .layer(tower_http::cors::CorsLayer::permissive())
        .with_state(state);

    let addr = format!("{host}:{port}");
    tracing::info!("Starting rho server on {}", addr);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
