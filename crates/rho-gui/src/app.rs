use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use iced::widget::markdown;
use iced::{keyboard, Subscription, Task as IcedTask};
use tokio_util::sync::CancellationToken;

use rho_core::agent_loop::{agent_loop, AgentLoopConfig};
use rho_core::tool::AgentTool;
use rho_core::types::*;

const SYSTEM_PROMPT: &str = "\
You are a coding assistant with tools for reading, editing, searching files and running commands.

Available tools:
- read: Read a file (returns LINE:HASH|content format) or list a directory
- write: Create or overwrite a file
- edit: Edit a file using LINE:HASH anchors from read output, or text replacement
- bash: Execute shell commands
- grep: Search file contents with regex (returns matches with LINE:HASH|content format)
- find: Find files by glob pattern (respects .gitignore)

When editing files, first read them to get LINE:HASH references, then use edit with those anchors. \
For new files, use write. For small changes, use edit. For running tests or builds, use bash.";

/// A block in the conversation view.
#[derive(Debug, Clone)]
pub enum ConversationBlock {
    UserPrompt(String),
    AssistantMarkdown {
        #[allow(dead_code)]
        raw: String,
        items: Vec<markdown::Item>,
    },
    ShellOutput {
        command: String,
        output: String,
        is_error: bool,
    },
    ToolCall(ToolCallBlock),
}

#[derive(Debug, Clone)]
pub struct ToolCallBlock {
    pub id: String,
    pub name: String,
    pub args: String,
    pub result: Option<String>,
    pub is_error: bool,
}

#[derive(Debug, Clone)]
pub struct ShellResult {
    pub command: String,
    pub output: String,
    pub is_error: bool,
}

/// Application state.
pub struct RhoApp {
    // Chat
    pub messages: Vec<ConversationBlock>,
    pub streaming_text: String,
    pub streaming_markdown: markdown::Content,
    pub input: String,
    pub is_running: bool,
    pub expanded_tools: HashSet<String>,
    pub error: Option<String>,
    // Agent infra
    cancel: CancellationToken,
    abort_handle: Option<iced::task::Handle>,
    api_key: Option<String>,
    pub cwd: PathBuf,
    pub model: Model,
    // Sidebar data
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub session_start: Instant,
}

/// Iced messages.
#[derive(Debug, Clone)]
pub enum Message {
    InputChanged(String),
    SendPrompt,
    AgentEvent(AgentEvent),
    CancelAgent,
    ShellDone(ShellResult),
    ToggleToolExpand(String),
    UrlClicked(markdown::Uri),
    Noop,
}

impl RhoApp {
    pub fn new() -> (Self, IcedTask<Message>) {
        let api_key = anthropic_auth::get_token().ok();
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

        let model = Model {
            id: "claude-sonnet-4-5-20250929".into(),
            name: "Sonnet 4.5".into(),
            provider: "anthropic".into(),
            base_url: String::new(),
            reasoning: false,
            context_window: 200_000,
            max_tokens: 8_192,
        };

        let app = Self {
            messages: Vec::new(),
            streaming_text: String::new(),
            streaming_markdown: markdown::Content::new(),
            input: String::new(),
            is_running: false,
            expanded_tools: HashSet::new(),
            error: if api_key.is_none() {
                Some("No API key found. Set ANTHROPIC_API_KEY or log in with Claude Code.".into())
            } else {
                None
            },
            cancel: CancellationToken::new(),
            abort_handle: None,
            api_key,
            cwd,
            model,
            total_input_tokens: 0,
            total_output_tokens: 0,
            session_start: Instant::now(),
        };

        (app, IcedTask::none())
    }

    pub fn update(&mut self, message: Message) -> IcedTask<Message> {
        match message {
            Message::InputChanged(text) => {
                self.input = text;
                IcedTask::none()
            }
            Message::SendPrompt => self.handle_send_prompt(),
            Message::AgentEvent(event) => {
                self.handle_agent_event(event);
                IcedTask::none()
            }
            Message::CancelAgent => {
                self.cancel.cancel();
                if let Some(handle) = self.abort_handle.take() {
                    handle.abort();
                }
                // Flush any streaming text
                if !self.streaming_text.is_empty() {
                    let raw = std::mem::take(&mut self.streaming_text);
                    let items = self.streaming_markdown.items().to_vec();
                    self.streaming_markdown = markdown::Content::new();
                    self.messages
                        .push(ConversationBlock::AssistantMarkdown { raw, items });
                }
                self.is_running = false;
                IcedTask::none()
            }
            Message::ShellDone(result) => {
                self.messages.push(ConversationBlock::ShellOutput {
                    command: result.command,
                    output: result.output,
                    is_error: result.is_error,
                });
                self.is_running = false;
                IcedTask::none()
            }
            Message::ToggleToolExpand(id) => {
                if !self.expanded_tools.remove(&id) {
                    self.expanded_tools.insert(id);
                }
                IcedTask::none()
            }
            Message::UrlClicked(_url) => {
                // Could open in browser; for now, no-op
                IcedTask::none()
            }
            Message::Noop => IcedTask::none(),
        }
    }

    fn handle_send_prompt(&mut self) -> IcedTask<Message> {
        if self.input.trim().is_empty() || self.is_running {
            return IcedTask::none();
        }

        let prompt = self.input.clone();
        self.input.clear();

        // Shell command (! prefix)
        if let Some(cmd) = prompt.strip_prefix('!') {
            let command = cmd.trim().to_string();
            if command.is_empty() {
                return IcedTask::none();
            }
            self.messages
                .push(ConversationBlock::UserPrompt(prompt.clone()));
            self.is_running = true;

            let cwd = self.cwd.clone();
            return IcedTask::perform(
                async move {
                    let bash = rho_tools::bash::BashTool::new(cwd);
                    let params = serde_json::json!({"command": &command});
                    let cancel = CancellationToken::new();
                    match bash.execute("shell", params, cancel).await {
                        Ok(tr) => ShellResult {
                            command,
                            output: extract_text(&tr.content),
                            is_error: false,
                        },
                        Err(e) => ShellResult {
                            command,
                            output: format!("{e}"),
                            is_error: true,
                        },
                    }
                },
                Message::ShellDone,
            );
        }

        // Normal agent prompt
        self.messages
            .push(ConversationBlock::UserPrompt(prompt.clone()));
        self.is_running = true;
        self.streaming_text.clear();
        self.streaming_markdown = markdown::Content::new();

        let api_key = match &self.api_key {
            Some(key) => key.clone(),
            None => {
                self.error = Some("No API key configured".into());
                self.is_running = false;
                return IcedTask::none();
            }
        };

        let cwd = self.cwd.clone();
        let cancel = CancellationToken::new();
        self.cancel = cancel.clone();

        let tools: Vec<Arc<dyn AgentTool>> = vec![
            Arc::new(rho_tools::read::ReadTool::with_cwd(cwd.clone())),
            Arc::new(rho_tools::write::WriteTool::with_cwd(cwd.clone())),
            Arc::new(rho_tools::edit::EditTool::with_cwd(cwd.clone())),
            Arc::new(rho_tools::bash::BashTool::new(cwd.clone())),
            Arc::new(rho_tools::grep::GrepTool::new(cwd.clone())),
            Arc::new(rho_tools::find::FindTool::new(cwd)),
        ];

        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        let prompts = vec![rho_core::types::Message::User {
            content: UserContent::Text(prompt),
            timestamp: now_ms,
        }];

        let config = AgentLoopConfig {
            model: self.model.clone(),
            api_key,
            system_prompt: SYSTEM_PROMPT.to_string(),
            tools,
            thinking: ThinkingLevel::Off,
            max_tokens: None,
            stream_fn: rho_provider::anthropic_stream_fn(),
            get_steering_messages: None,
            get_follow_up_messages: None,
        };

        let consumer = agent_loop(prompts, config, cancel);
        let (task, handle) = IcedTask::run(consumer, Message::AgentEvent).abortable();
        self.abort_handle = Some(handle);
        task
    }

    fn handle_agent_event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::MessageUpdate {
                event: AssistantStreamEvent::TextDelta { delta, .. },
                ..
            } => {
                self.streaming_text.push_str(&delta);
                self.streaming_markdown.push_str(&delta);
            }
            AgentEvent::MessageEnd { message, .. } => {
                // Accumulate token usage
                if let rho_core::types::Message::Assistant { usage, .. } = &message {
                    self.total_input_tokens += usage.input;
                    self.total_output_tokens += usage.output;
                }
                // Flush streaming text to a markdown block
                if !self.streaming_text.is_empty() {
                    let raw = std::mem::take(&mut self.streaming_text);
                    let items = self.streaming_markdown.items().to_vec();
                    self.streaming_markdown = markdown::Content::new();
                    self.messages
                        .push(ConversationBlock::AssistantMarkdown { raw, items });
                }
            }
            AgentEvent::ToolExecutionStart {
                tool_call_id,
                tool_name,
                args,
                ..
            } => {
                self.messages
                    .push(ConversationBlock::ToolCall(ToolCallBlock {
                        id: tool_call_id,
                        name: tool_name,
                        args: serde_json::to_string_pretty(&args).unwrap_or_default(),
                        result: None,
                        is_error: false,
                    }));
            }
            AgentEvent::ToolExecutionEnd {
                tool_call_id,
                result,
                is_error,
                ..
            } => {
                for block in self.messages.iter_mut().rev() {
                    if let ConversationBlock::ToolCall(tc) = block {
                        if tc.id == tool_call_id {
                            tc.result = Some(extract_text(&result.content));
                            tc.is_error = is_error;
                            break;
                        }
                    }
                }
            }
            AgentEvent::AgentEnd { .. } => {
                self.is_running = false;
                self.abort_handle = None;
            }
            _ => {}
        }
    }
}

pub fn subscription(app: &RhoApp) -> Subscription<Message> {
    if app.is_running {
        keyboard::listen().map(|event| match event {
            keyboard::Event::KeyPressed {
                key: keyboard::Key::Named(keyboard::key::Named::Escape),
                ..
            } => Message::CancelAgent,
            _ => Message::Noop,
        })
    } else {
        Subscription::none()
    }
}

/// Extract text content from a Vec<Content> for display.
fn extract_text(content: &[Content]) -> String {
    content
        .iter()
        .filter_map(|c| match c {
            Content::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_changed_updates_field() {
        let (mut app, _) = RhoApp::new();
        let _ = app.update(Message::InputChanged("hello".into()));
        assert_eq!(app.input, "hello");
    }

    #[test]
    fn send_prompt_ignores_empty_input() {
        let (mut app, _) = RhoApp::new();
        let _ = app.update(Message::SendPrompt);
        assert!(app.messages.is_empty());
    }

    #[test]
    fn send_prompt_ignores_while_running() {
        let (mut app, _) = RhoApp::new();
        app.is_running = true;
        let _ = app.update(Message::InputChanged("test".into()));
        let _ = app.update(Message::SendPrompt);
        assert!(app.messages.is_empty());
    }

    #[test]
    fn text_delta_appends_to_streaming() {
        let (mut app, _) = RhoApp::new();
        let event = AgentEvent::MessageUpdate {
            message: rho_core::types::Message::Assistant {
                content: vec![],
                model: String::new(),
                usage: Usage::default(),
                stop_reason: StopReason::Stop,
                timestamp: 0,
            },
            event: AssistantStreamEvent::TextDelta {
                index: 0,
                delta: "hello ".into(),
            },
        };
        let _ = app.update(Message::AgentEvent(event));
        assert_eq!(app.streaming_text, "hello ");

        let event2 = AgentEvent::MessageUpdate {
            message: rho_core::types::Message::Assistant {
                content: vec![],
                model: String::new(),
                usage: Usage::default(),
                stop_reason: StopReason::Stop,
                timestamp: 0,
            },
            event: AssistantStreamEvent::TextDelta {
                index: 0,
                delta: "world".into(),
            },
        };
        let _ = app.update(Message::AgentEvent(event2));
        assert_eq!(app.streaming_text, "hello world");
    }

    #[test]
    fn message_end_flushes_streaming_text() {
        let (mut app, _) = RhoApp::new();
        app.streaming_text = "accumulated text".into();
        app.streaming_markdown = markdown::Content::parse("accumulated text");

        let event = AgentEvent::MessageEnd {
            message: rho_core::types::Message::Assistant {
                content: vec![],
                model: String::new(),
                usage: Usage::default(),
                stop_reason: StopReason::Stop,
                timestamp: 0,
            },
        };
        let _ = app.update(Message::AgentEvent(event));

        assert!(app.streaming_text.is_empty());
        assert!(matches!(
            &app.messages[0],
            ConversationBlock::AssistantMarkdown { raw, .. } if raw == "accumulated text"
        ));
    }

    #[test]
    fn toggle_tool_expand() {
        let (mut app, _) = RhoApp::new();
        let _ = app.update(Message::ToggleToolExpand("id1".into()));
        assert!(app.expanded_tools.contains("id1"));

        let _ = app.update(Message::ToggleToolExpand("id1".into()));
        assert!(!app.expanded_tools.contains("id1"));
    }

    #[test]
    fn cancel_agent_clears_running() {
        let (mut app, _) = RhoApp::new();
        app.is_running = true;
        let _ = app.update(Message::CancelAgent);
        assert!(!app.is_running);
    }

    #[test]
    fn shell_done_adds_block() {
        let (mut app, _) = RhoApp::new();
        app.is_running = true;
        let _ = app.update(Message::ShellDone(ShellResult {
            command: "ls".into(),
            output: "file.txt".into(),
            is_error: false,
        }));
        assert!(!app.is_running);
        assert!(matches!(
            &app.messages[0],
            ConversationBlock::ShellOutput { command, .. } if command == "ls"
        ));
    }

    #[test]
    fn message_end_accumulates_usage() {
        let (mut app, _) = RhoApp::new();
        app.streaming_text = "text".into();
        app.streaming_markdown = markdown::Content::parse("text");

        let event = AgentEvent::MessageEnd {
            message: rho_core::types::Message::Assistant {
                content: vec![],
                model: String::new(),
                usage: Usage {
                    input: 100,
                    output: 50,
                    cache_read: 0,
                    cache_write: 0,
                },
                stop_reason: StopReason::Stop,
                timestamp: 0,
            },
        };
        let _ = app.update(Message::AgentEvent(event));
        assert_eq!(app.total_input_tokens, 100);
        assert_eq!(app.total_output_tokens, 50);
    }
}
