use std::path::Path;
use std::sync::Mutex;

use chrono::Utc;
use rusqlite::{params, Connection};
use uuid::Uuid;

use crate::types::Message;

pub struct SessionStore {
    conn: Mutex<Connection>,
}

pub struct Session {
    pub id: String,
    pub title: String,
    pub model: String,
    pub created_at: String,
    pub cwd: String,
}

#[derive(Debug, Clone)]
pub struct SessionSummary {
    pub id: String,
    pub title: String,
    pub model: String,
    pub updated_at: String,
    pub message_count: usize,
}

impl SessionStore {
    pub fn open(path: &Path) -> Result<Self, rusqlite::Error> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                model TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                cwd TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL REFERENCES sessions(id),
                role TEXT NOT NULL,
                content_json TEXT NOT NULL,
                timestamp INTEGER NOT NULL,
                seq INTEGER NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id, seq);",
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn create_session(&self, model: &str, cwd: &Path) -> Result<Session, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        let cwd_str = cwd.display().to_string();
        let title = "New session".to_string();

        conn.execute(
            "INSERT INTO sessions (id, title, model, created_at, updated_at, cwd)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![id, title, model, now, now, cwd_str],
        )?;

        Ok(Session {
            id,
            title,
            model: model.to_string(),
            created_at: now.clone(),
            cwd: cwd_str,
        })
    }

    pub fn update_title(&self, id: &str, title: &str) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE sessions SET title = ?1, updated_at = ?2 WHERE id = ?3",
            params![title, Utc::now().to_rfc3339(), id],
        )?;
        Ok(())
    }

    pub fn save_messages(
        &self,
        session_id: &str,
        messages: &[Message],
    ) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let tx = conn.unchecked_transaction()?;

        tx.execute(
            "DELETE FROM messages WHERE session_id = ?1",
            params![session_id],
        )?;

        let now = Utc::now().to_rfc3339();
        tx.execute(
            "UPDATE sessions SET updated_at = ?1 WHERE id = ?2",
            params![now, session_id],
        )?;

        for (seq, msg) in messages.iter().enumerate() {
            let role = match msg {
                Message::User { .. } => "user",
                Message::Assistant { .. } => "assistant",
                Message::ToolResult { .. } => "toolResult",
            };
            let content_json = serde_json::to_string(msg).unwrap_or_default();
            let timestamp = match msg {
                Message::User { timestamp, .. } => *timestamp,
                Message::Assistant { timestamp, .. } => *timestamp,
                Message::ToolResult { timestamp, .. } => *timestamp,
            };

            tx.execute(
                "INSERT INTO messages (session_id, role, content_json, timestamp, seq)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![session_id, role, content_json, timestamp as i64, seq as i64],
            )?;
        }

        tx.commit()?;
        Ok(())
    }

    pub fn load_messages(&self, session_id: &str) -> Result<Vec<Message>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let mut stmt =
            conn.prepare("SELECT content_json FROM messages WHERE session_id = ?1 ORDER BY seq")?;
        let messages = stmt
            .query_map(params![session_id], |row| {
                let json: String = row.get(0)?;
                Ok(json)
            })?
            .filter_map(|r| r.ok())
            .filter_map(|json| serde_json::from_str::<Message>(&json).ok())
            .collect();
        Ok(messages)
    }

    pub fn session_exists(&self, session_id: &str) -> Result<bool, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT COUNT(1) FROM sessions WHERE id = ?1")?;
        let count: i64 = stmt.query_row(params![session_id], |row| row.get(0))?;
        Ok(count > 0)
    }

    pub fn list_sessions(&self, limit: usize) -> Result<Vec<SessionSummary>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT s.id, s.title, s.model, s.updated_at,
                    (SELECT COUNT(*) FROM messages m WHERE m.session_id = s.id) as msg_count
             FROM sessions s
             ORDER BY s.updated_at DESC
             LIMIT ?1",
        )?;
        let sessions = stmt
            .query_map(params![limit as i64], |row| {
                Ok(SessionSummary {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    model: row.get(2)?,
                    updated_at: row.get(3)?,
                    message_count: row.get::<_, i64>(4)? as usize,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();
        Ok(sessions)
    }

    pub fn delete_session(&self, id: &str) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let tx = conn.unchecked_transaction()?;
        tx.execute("DELETE FROM messages WHERE session_id = ?1", params![id])?;
        tx.execute("DELETE FROM sessions WHERE id = ?1", params![id])?;
        tx.commit()?;
        Ok(())
    }
}

impl crate::event_handler::SessionPersistence for SessionStore {
    fn create_session(
        &self,
        model: &str,
        cwd: &std::path::Path,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let session = SessionStore::create_session(self, model, cwd)?;
        Ok(session.id)
    }

    fn update_title(
        &self,
        session_id: &str,
        title: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        SessionStore::update_title(self, session_id, title)?;
        Ok(())
    }

    fn save_messages(
        &self,
        session_id: &str,
        messages: &[Message],
    ) -> Result<(), Box<dyn std::error::Error>> {
        SessionStore::save_messages(self, session_id, messages)?;
        Ok(())
    }
}

/// Extract a session title from conversation messages.
/// Uses the first user message text, truncated to 80 chars.
pub fn extract_title(messages: &[Message]) -> String {
    for msg in messages {
        if let Message::User {
            content: crate::types::UserContent::Text(text),
            ..
        } = msg
        {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                continue;
            }
            // Take first line, truncate to 80 chars
            let first_line = trimmed.lines().next().unwrap_or(trimmed);
            if first_line.len() > 80 {
                return format!("{}...", &first_line[..77]);
            }
            return first_line.to_string();
        }
    }
    "Untitled session".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::*;

    fn test_store() -> SessionStore {
        SessionStore::open(Path::new(":memory:")).unwrap()
    }

    #[test]
    fn create_and_list_sessions() {
        let store = test_store();
        let s1 = store
            .create_session("claude-sonnet", Path::new("/tmp"))
            .unwrap();
        let s2 = store
            .create_session("claude-opus", Path::new("/tmp"))
            .unwrap();

        let list = store.list_sessions(50).unwrap();
        assert_eq!(list.len(), 2);
        // Most recent first
        assert_eq!(list[0].id, s2.id);
        assert_eq!(list[1].id, s1.id);
    }

    #[test]
    fn save_and_load_messages() {
        let store = test_store();
        let session = store
            .create_session("claude-sonnet", Path::new("/tmp"))
            .unwrap();

        let messages = vec![
            Message::User {
                content: UserContent::Text("hello".into()),
                timestamp: 1000,
            },
            Message::Assistant {
                content: vec![Content::Text {
                    text: "hi there".into(),
                }],
                model: "claude-sonnet".into(),
                usage: Usage::default(),
                stop_reason: StopReason::Stop,
                timestamp: 1001,
            },
        ];

        store.save_messages(&session.id, &messages).unwrap();

        let loaded = store.load_messages(&session.id).unwrap();
        assert_eq!(loaded.len(), 2);

        match &loaded[0] {
            Message::User {
                content: UserContent::Text(t),
                ..
            } => assert_eq!(t, "hello"),
            _ => panic!("expected User message"),
        }
        match &loaded[1] {
            Message::Assistant { content, .. } => {
                assert_eq!(content.len(), 1);
            }
            _ => panic!("expected Assistant message"),
        }
    }

    #[test]
    fn session_exists_returns_true_for_known_session() {
        let store = test_store();
        let session = store
            .create_session("claude-sonnet", Path::new("/tmp"))
            .unwrap();
        assert!(store.session_exists(&session.id).unwrap());
    }

    #[test]
    fn session_exists_returns_false_for_unknown_session() {
        let store = test_store();
        assert!(!store.session_exists("does-not-exist").unwrap());
    }

    #[test]
    fn save_messages_replaces() {
        let store = test_store();
        let session = store
            .create_session("claude-sonnet", Path::new("/tmp"))
            .unwrap();

        let messages1 = vec![Message::User {
            content: UserContent::Text("first".into()),
            timestamp: 1000,
        }];
        store.save_messages(&session.id, &messages1).unwrap();

        let messages2 = vec![
            Message::User {
                content: UserContent::Text("second".into()),
                timestamp: 2000,
            },
            Message::User {
                content: UserContent::Text("third".into()),
                timestamp: 2001,
            },
        ];
        store.save_messages(&session.id, &messages2).unwrap();

        let loaded = store.load_messages(&session.id).unwrap();
        assert_eq!(loaded.len(), 2);
    }

    #[test]
    fn delete_session() {
        let store = test_store();
        let session = store
            .create_session("claude-sonnet", Path::new("/tmp"))
            .unwrap();

        let messages = vec![Message::User {
            content: UserContent::Text("hello".into()),
            timestamp: 1000,
        }];
        store.save_messages(&session.id, &messages).unwrap();

        store.delete_session(&session.id).unwrap();
        let list = store.list_sessions(50).unwrap();
        assert!(list.is_empty());

        let loaded = store.load_messages(&session.id).unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn update_title() {
        let store = test_store();
        let session = store
            .create_session("claude-sonnet", Path::new("/tmp"))
            .unwrap();

        store.update_title(&session.id, "My Session").unwrap();

        let list = store.list_sessions(50).unwrap();
        assert_eq!(list[0].title, "My Session");
    }

    #[test]
    fn list_sessions_with_message_count() {
        let store = test_store();
        let session = store
            .create_session("claude-sonnet", Path::new("/tmp"))
            .unwrap();

        let messages = vec![
            Message::User {
                content: UserContent::Text("hello".into()),
                timestamp: 1000,
            },
            Message::Assistant {
                content: vec![Content::Text { text: "hi".into() }],
                model: "claude-sonnet".into(),
                usage: Usage::default(),
                stop_reason: StopReason::Stop,
                timestamp: 1001,
            },
        ];
        store.save_messages(&session.id, &messages).unwrap();

        let list = store.list_sessions(50).unwrap();
        assert_eq!(list[0].message_count, 2);
    }

    #[test]
    fn extract_title_from_messages() {
        let messages = vec![Message::User {
            content: UserContent::Text("Help me write a function".into()),
            timestamp: 0,
        }];
        assert_eq!(extract_title(&messages), "Help me write a function");
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

    #[test]
    fn extract_title_empty() {
        let messages: Vec<Message> = vec![];
        assert_eq!(extract_title(&messages), "Untitled session");
    }

    #[test]
    fn extract_title_skips_empty_user_messages() {
        let messages = vec![
            Message::User {
                content: UserContent::Text("".into()),
                timestamp: 0,
            },
            Message::User {
                content: UserContent::Text("Real message".into()),
                timestamp: 1,
            },
        ];
        assert_eq!(extract_title(&messages), "Real message");
    }
}
