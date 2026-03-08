use std::path::Path;
use std::sync::Mutex;

use chrono::Utc;
use rusqlite::{params, Connection};
use uuid::Uuid;

use rho_core::types::Message;

pub struct SessionStore {
    conn: Mutex<Connection>,
}

pub struct Session {
    pub id: String,
    pub title: String,
    pub model: String,
    pub created_at: String,
    pub cwd: String,
    pub parent_id: Option<String>,
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
                cwd TEXT NOT NULL,
                parent_id TEXT
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
        // Migration: add parent_id column if it doesn't exist (for pre-existing databases)
        conn.execute_batch(
            "ALTER TABLE sessions ADD COLUMN parent_id TEXT;",
        ).ok(); // Ignore error if column already exists
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn create_session(&self, model: &str, cwd: &Path) -> Result<Session, rusqlite::Error> {
        self.create_session_with_parent(model, cwd, None)
    }

    pub fn create_session_with_parent(
        &self,
        model: &str,
        cwd: &Path,
        parent_id: Option<&str>,
    ) -> Result<Session, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        let cwd_str = cwd.display().to_string();
        let title = "New session".to_string();

        conn.execute(
            "INSERT INTO sessions (id, title, model, created_at, updated_at, cwd, parent_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![id, title, model, now, now, cwd_str, parent_id],
        )?;

        Ok(Session {
            id,
            title,
            model: model.to_string(),
            created_at: now.clone(),
            cwd: cwd_str,
            parent_id: parent_id.map(String::from),
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

    /// Create a new session that branches from an existing parent session.
    /// All messages from the parent are copied into the new session.
    /// Returns the new session's ID.
    pub fn branch_session(&self, parent_id: &str) -> Result<String, rusqlite::Error> {
        // First, load parent session metadata
        let (model, cwd) = {
            let conn = self.conn.lock().unwrap();
            let mut stmt =
                conn.prepare("SELECT model, cwd FROM sessions WHERE id = ?1")?;
            stmt.query_row(params![parent_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
        };

        // Load parent messages
        let parent_messages = self.load_messages(parent_id)?;

        // Create the new session with parent_id set
        let new_session = self.create_session_with_parent(
            &model,
            Path::new(&cwd),
            Some(parent_id),
        )?;

        // Copy parent messages into the new session
        if !parent_messages.is_empty() {
            self.save_messages(&new_session.id, &parent_messages)?;
        }

        Ok(new_session.id)
    }

    /// Load messages from a session and all its ancestors (via parent_id chain),
    /// in chronological order (oldest ancestor first).
    pub fn load_session_with_parents(
        &self,
        id: &str,
    ) -> Result<Vec<Message>, rusqlite::Error> {
        // Walk the parent chain to collect session IDs from root to leaf
        let mut chain = Vec::new();
        let mut current_id = Some(id.to_string());

        while let Some(ref sid) = current_id {
            chain.push(sid.clone());
            let conn = self.conn.lock().unwrap();
            let mut stmt =
                conn.prepare("SELECT parent_id FROM sessions WHERE id = ?1")?;
            let parent: Option<String> = stmt
                .query_row(params![sid], |row| row.get(0))
                .unwrap_or(None);
            current_id = parent;
        }

        // Reverse so we go from root ancestor to the current session
        chain.reverse();

        // Collect messages from each session in order
        let mut all_messages = Vec::new();
        for session_id in &chain {
            let msgs = self.load_messages(session_id)?;
            all_messages.extend(msgs);
        }

        Ok(all_messages)
    }
}

impl rho_core::event_handler::SessionPersistence for SessionStore {
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
            content: rho_core::types::UserContent::Text(text),
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
    use rho_core::types::*;

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

    #[test]
    fn branch_session_copies_messages() {
        let store = test_store();
        let parent = store
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
        store.save_messages(&parent.id, &messages).unwrap();

        let branch_id = store.branch_session(&parent.id).unwrap();

        // Branch should be a different session
        assert_ne!(branch_id, parent.id);
        assert!(store.session_exists(&branch_id).unwrap());

        // Branch should have all parent messages copied
        let branch_messages = store.load_messages(&branch_id).unwrap();
        assert_eq!(branch_messages.len(), 2);

        match &branch_messages[0] {
            Message::User {
                content: UserContent::Text(t),
                ..
            } => assert_eq!(t, "hello"),
            _ => panic!("expected User message"),
        }

        // Parent messages should still be intact
        let parent_messages = store.load_messages(&parent.id).unwrap();
        assert_eq!(parent_messages.len(), 2);
    }

    #[test]
    fn branch_session_sets_parent_id() {
        let store = test_store();
        let parent = store
            .create_session("claude-sonnet", Path::new("/tmp"))
            .unwrap();

        let messages = vec![Message::User {
            content: UserContent::Text("hello".into()),
            timestamp: 1000,
        }];
        store.save_messages(&parent.id, &messages).unwrap();

        let branch_id = store.branch_session(&parent.id).unwrap();

        // Verify the parent_id is set by checking via SQL
        let conn = store.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT parent_id FROM sessions WHERE id = ?1")
            .unwrap();
        let stored_parent: Option<String> = stmt
            .query_row(params![branch_id], |row| row.get(0))
            .unwrap();
        assert_eq!(stored_parent.as_deref(), Some(parent.id.as_str()));
    }

    #[test]
    fn branch_session_from_empty_parent() {
        let store = test_store();
        let parent = store
            .create_session("claude-sonnet", Path::new("/tmp"))
            .unwrap();

        // Branch from a session with no messages
        let branch_id = store.branch_session(&parent.id).unwrap();
        let branch_messages = store.load_messages(&branch_id).unwrap();
        assert!(branch_messages.is_empty());
    }

    #[test]
    fn load_session_with_parents_single_session() {
        let store = test_store();
        let session = store
            .create_session("claude-sonnet", Path::new("/tmp"))
            .unwrap();

        let messages = vec![Message::User {
            content: UserContent::Text("hello".into()),
            timestamp: 1000,
        }];
        store.save_messages(&session.id, &messages).unwrap();

        // For a root session, load_session_with_parents should return just its own messages
        let loaded = store.load_session_with_parents(&session.id).unwrap();
        assert_eq!(loaded.len(), 1);
    }

    #[test]
    fn load_session_with_parents_chain() {
        let store = test_store();

        // Create root session with messages
        let root = store
            .create_session("claude-sonnet", Path::new("/tmp"))
            .unwrap();
        let root_messages = vec![Message::User {
            content: UserContent::Text("root message".into()),
            timestamp: 1000,
        }];
        store.save_messages(&root.id, &root_messages).unwrap();

        // Branch from root
        let branch1_id = store.branch_session(&root.id).unwrap();
        // Add a new message to branch1
        let mut branch1_msgs = store.load_messages(&branch1_id).unwrap();
        branch1_msgs.push(Message::User {
            content: UserContent::Text("branch1 message".into()),
            timestamp: 2000,
        });
        store.save_messages(&branch1_id, &branch1_msgs).unwrap();

        // Branch from branch1
        let branch2_id = store.branch_session(&branch1_id).unwrap();
        // Add a new message to branch2
        let mut branch2_msgs = store.load_messages(&branch2_id).unwrap();
        branch2_msgs.push(Message::User {
            content: UserContent::Text("branch2 message".into()),
            timestamp: 3000,
        });
        store.save_messages(&branch2_id, &branch2_msgs).unwrap();

        // load_session_with_parents on branch2 should return messages from
        // root + branch1 + branch2 in order
        let all = store.load_session_with_parents(&branch2_id).unwrap();

        // root has 1 msg, branch1 has 2 (copied root + its own), branch2 has 3 (copied + its own)
        // The chain walks: root -> branch1 -> branch2
        // root messages: ["root message"]
        // branch1 messages: ["root message", "branch1 message"]
        // branch2 messages: ["root message", "branch1 message", "branch2 message"]
        // Total concatenated: 1 + 2 + 3 = 6
        assert_eq!(all.len(), 6);

        // First message should be from the root
        match &all[0] {
            Message::User {
                content: UserContent::Text(t),
                ..
            } => assert_eq!(t, "root message"),
            _ => panic!("expected root User message"),
        }

        // Last message should be from branch2
        match &all[5] {
            Message::User {
                content: UserContent::Text(t),
                ..
            } => assert_eq!(t, "branch2 message"),
            _ => panic!("expected branch2 User message"),
        }
    }
}
