use std::sync::Arc;

use crate::types::{AgentEvent, Message, UserContent};

/// Configuration for the shared event handler.
/// Both GUI and CLI provide their own config with the session store + current session state.
pub struct EventHandlerConfig {
    /// If set, events will trigger session persistence.
    pub session_store: Option<Arc<dyn SessionPersistence>>,
    /// Current session ID (None if no session yet).
    pub session_id: Option<String>,
    /// Model name (used when creating new sessions).
    pub model_id: String,
    /// Working directory (used when creating new sessions).
    pub cwd: std::path::PathBuf,
}

/// Result from processing an event. Contains any state updates the caller should apply.
pub struct EventHandlerResult {
    /// Updated session ID (set when a session is created or changed).
    pub session_id: Option<String>,
    /// Extracted session title.
    pub session_title: Option<String>,
}

/// Trait for session persistence, so the event handler doesn't depend on rho-session directly.
pub trait SessionPersistence: Send + Sync {
    fn create_session(
        &self,
        model: &str,
        cwd: &std::path::Path,
    ) -> Result<String, Box<dyn std::error::Error>>;
    fn update_title(
        &self,
        session_id: &str,
        title: &str,
    ) -> Result<(), Box<dyn std::error::Error>>;
    fn save_messages(
        &self,
        session_id: &str,
        messages: &[Message],
    ) -> Result<(), Box<dyn std::error::Error>>;
}

/// Extract a session title from conversation messages.
fn extract_title(messages: &[Message]) -> String {
    for msg in messages {
        if let Message::User {
            content: UserContent::Text(text),
            ..
        } = msg
        {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                continue;
            }
            let first_line = trimmed.lines().next().unwrap_or(trimmed);
            if first_line.len() > 80 {
                let end = first_line
                    .char_indices()
                    .map(|(i, _)| i)
                    .take_while(|&i| i <= 77)
                    .last()
                    .unwrap_or(0);
                return format!("{}...", &first_line[..end]);
            }
            return first_line.to_string();
        }
    }
    "Untitled session".to_string()
}

/// Process an agent event, performing any side effects (persistence, etc).
/// Returns state updates the caller should apply, or None if no action needed.
pub fn handle_event(
    event: &AgentEvent,
    config: &mut EventHandlerConfig,
) -> Option<EventHandlerResult> {
    match event {
        AgentEvent::AgentEnd { messages, .. } => {
            let store = config.session_store.as_ref()?;

            if messages.is_empty() {
                return None;
            }

            // Create session on first AgentEnd if none exists
            let session_id = if let Some(ref id) = config.session_id {
                id.clone()
            } else {
                match store.create_session(&config.model_id, &config.cwd) {
                    Ok(id) => {
                        config.session_id = Some(id.clone());
                        id
                    }
                    Err(_) => return None,
                }
            };

            // Update title and save messages
            let title = extract_title(messages);
            store.update_title(&session_id, &title).ok();
            store.save_messages(&session_id, messages).ok();

            Some(EventHandlerResult {
                session_id: Some(session_id),
                session_title: Some(title),
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::*;
    use std::path::PathBuf;
    use std::sync::Mutex;

    struct MockStore {
        sessions: Mutex<Vec<(String, String)>>,
        saved_messages: Mutex<Vec<(String, usize)>>,
    }

    impl MockStore {
        fn new() -> Self {
            Self {
                sessions: Mutex::new(Vec::new()),
                saved_messages: Mutex::new(Vec::new()),
            }
        }
    }

    impl SessionPersistence for MockStore {
        fn create_session(
            &self,
            _model: &str,
            _cwd: &std::path::Path,
        ) -> Result<String, Box<dyn std::error::Error>> {
            let mut sessions = self.sessions.lock().unwrap();
            let id = format!("session-{}", sessions.len());
            sessions.push((id.clone(), String::new()));
            Ok(id)
        }

        fn update_title(
            &self,
            session_id: &str,
            title: &str,
        ) -> Result<(), Box<dyn std::error::Error>> {
            let mut sessions = self.sessions.lock().unwrap();
            for (id, t) in sessions.iter_mut() {
                if id == session_id {
                    *t = title.to_string();
                }
            }
            Ok(())
        }

        fn save_messages(
            &self,
            session_id: &str,
            messages: &[Message],
        ) -> Result<(), Box<dyn std::error::Error>> {
            self.saved_messages
                .lock()
                .unwrap()
                .push((session_id.to_string(), messages.len()));
            Ok(())
        }
    }

    #[test]
    fn handle_event_creates_session_on_first_agent_end() {
        let store = Arc::new(MockStore::new());
        let mut config = EventHandlerConfig {
            session_store: Some(store.clone()),
            session_id: None,
            model_id: "claude-sonnet".into(),
            cwd: PathBuf::from("/tmp"),
        };

        let messages = vec![Message::User {
            content: UserContent::Text("hello".into()),
            timestamp: 1000,
        }];
        let event = AgentEvent::AgentEnd {
            messages: messages.clone(),
        };

        let result = handle_event(&event, &mut config).unwrap();
        assert!(result.session_id.is_some());
        assert_eq!(result.session_title, Some("hello".into()));
        assert_eq!(store.sessions.lock().unwrap().len(), 1);
        assert_eq!(store.saved_messages.lock().unwrap().len(), 1);
    }

    #[test]
    fn handle_event_reuses_existing_session() {
        let store = Arc::new(MockStore::new());
        let mut config = EventHandlerConfig {
            session_store: Some(store.clone()),
            session_id: Some("existing-session".into()),
            model_id: "claude-sonnet".into(),
            cwd: PathBuf::from("/tmp"),
        };

        let messages = vec![Message::User {
            content: UserContent::Text("hello".into()),
            timestamp: 1000,
        }];
        let event = AgentEvent::AgentEnd { messages };

        let result = handle_event(&event, &mut config).unwrap();
        assert_eq!(result.session_id, Some("existing-session".into()));
        // Should not create a new session
        assert_eq!(store.sessions.lock().unwrap().len(), 0);
    }

    #[test]
    fn handle_event_ignores_non_agent_end() {
        let store = Arc::new(MockStore::new());
        let mut config = EventHandlerConfig {
            session_store: Some(store),
            session_id: None,
            model_id: "claude-sonnet".into(),
            cwd: PathBuf::from("/tmp"),
        };

        let result = handle_event(&AgentEvent::AgentStart, &mut config);
        assert!(result.is_none());
    }

    #[test]
    fn handle_event_no_store_returns_none() {
        let mut config = EventHandlerConfig {
            session_store: None,
            session_id: None,
            model_id: "claude-sonnet".into(),
            cwd: PathBuf::from("/tmp"),
        };

        let messages = vec![Message::User {
            content: UserContent::Text("hello".into()),
            timestamp: 1000,
        }];
        let event = AgentEvent::AgentEnd { messages };

        let result = handle_event(&event, &mut config);
        assert!(result.is_none());
    }

    #[test]
    fn handle_event_empty_messages_returns_none() {
        let store = Arc::new(MockStore::new());
        let mut config = EventHandlerConfig {
            session_store: Some(store),
            session_id: None,
            model_id: "claude-sonnet".into(),
            cwd: PathBuf::from("/tmp"),
        };

        let event = AgentEvent::AgentEnd {
            messages: Vec::new(),
        };
        let result = handle_event(&event, &mut config);
        assert!(result.is_none());
    }

    #[test]
    fn extract_title_basic() {
        let messages = vec![Message::User {
            content: UserContent::Text("Help me write code".into()),
            timestamp: 0,
        }];
        assert_eq!(extract_title(&messages), "Help me write code");
    }

    #[test]
    fn extract_title_truncates() {
        let long = "a".repeat(100);
        let messages = vec![Message::User {
            content: UserContent::Text(long),
            timestamp: 0,
        }];
        let title = extract_title(&messages);
        assert!(title.len() <= 80);
        assert!(title.ends_with("..."));
    }
}
