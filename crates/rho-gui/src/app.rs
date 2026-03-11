use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use iced::widget::markdown;
use iced::{event, keyboard, Subscription, Task as IcedTask};
use tokio_util::sync::CancellationToken;

use rho_core::agent_loop::{agent_loop, AgentLoopConfig};
use rho_core::compaction;
use rho_core::config::ProjectConfig;
use rho_core::memories::MemoryMetadata;
use rho_core::models::{ModelConfig, ModelRegistry, ProviderType};
use rho_core::skills::SkillMetadata;
use rho_core::tool::AgentTool;
use rho_core::types::*;

use rho_core::event_handler::{EventHandlerConfig, EventHandlerResult};
use rho_core::session::SessionStore;

use crate::autocomplete::{self, AutocompleteState, AutocompleteTrigger};

pub const INPUT_ID: &str = "rho-input";

fn build_system_prompt_with_tools(tools: &[(Arc<dyn AgentTool>, bool)]) -> String {
    let now = chrono::Local::now();
    let current_date = now.format("%Y-%m-%d %H:%M %Z").to_string();
    let current_month_year = now.format("%B %Y").to_string();
    let current_year = now.format("%Y").to_string();

    let tool_docs: String = tools
        .iter()
        .filter(|(_, enabled)| *enabled)
        .map(|(t, _)| format_tool_doc(t.as_ref()))
        .collect::<Vec<_>>()
        .join("\n\n");

    format!(
        "\
You are a coding assistant with tools for reading, editing, searching files and running commands.

The current date is {current_date}. The current month is {current_month_year}. \
You MUST use this year when searching for recent information, documentation, or current events.

# Tools

{tool_docs}

# Guidelines

- To edit a file: read it first, then call edit with the exact text to replace.
- To create a new file: use write.
- For builds, tests, git: use bash.
- Web search: always include the year {current_year} in queries for recent info. \
Use multiple searches, then fetch primary sources with web_fetch.",
        current_date = current_date,
        current_month_year = current_month_year,
        current_year = current_year,
        tool_docs = tool_docs,
    )
}

/// Format a single tool's documentation block from its name, description, and schema.
fn format_tool_doc(tool: &dyn AgentTool) -> String {
    let schema = tool.parameters_schema();
    let props = schema.get("properties").and_then(|p| p.as_object());
    let required: Vec<&str> = schema
        .get("required")
        .and_then(|r| r.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();

    let mut params = String::new();
    if let Some(props) = props {
        for (name, prop) in props {
            let ty = prop.get("type").and_then(|v| v.as_str()).unwrap_or("any");
            let desc = prop
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let req = if required.contains(&name.as_str()) {
                "required"
            } else {
                "optional"
            };
            params.push_str(&format!("  - {name} ({ty}, {req}): {desc}\n"));
        }
    }

    format!(
        "## {name}\n{desc}\nParameters:\n{params}",
        name = tool.name(),
        desc = tool.description(),
        params = params.trim_end(),
    )
}

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
    ToolSummary(Vec<(String, usize)>),
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
    pub autocomplete: AutocompleteState,
    pub skills: Vec<SkillMetadata>,
    pub memories: Vec<MemoryMetadata>,
    // Multi-turn conversation history
    pub conversation_history: Vec<rho_core::types::Message>,
    // Command history
    pub history: Vec<String>,
    pub history_index: Option<usize>,
    history_draft: String,
    // Agent infra
    cancel: CancellationToken,
    abort_handle: Option<iced::task::Handle>,
    api_key: Option<String>,
    pub cwd: PathBuf,
    pub model: Model,
    pub model_config: ModelConfig,
    pub model_registry: ModelRegistry,
    pub project_config: ProjectConfig,
    // Tools (modular activation)
    pub available_tools: Vec<(Arc<dyn AgentTool>, bool)>,
    // Claude proxy for web tools (shared with tool instances)
    pub claude_proxy: Arc<AtomicBool>,
    // Tool summary tracking per turn
    pub turn_tool_counts: HashMap<String, usize>,
    // Session persistence
    pub session_store: Arc<SessionStore>,
    pub current_session_id: Option<String>,
    pub session_list: Vec<rho_core::session::SessionSummary>,
    pub event_handler_config: EventHandlerConfig,
    // Sidebar data
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub session_start: Instant,
    // Settings panel
    pub settings_open: bool,
    pub settings_tab: SettingsTab,
    pub project_config_content: iced::widget::text_editor::Content,
    pub models_config_content: iced::widget::text_editor::Content,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SettingsTab {
    Project,
    Models,
}

/// Iced messages.
#[derive(Debug, Clone)]
pub enum Message {
    InputChanged(String),
    SendPrompt,
    AgentEvent(AgentEvent),
    CancelAgent,
    ShellDone(ShellResult),
    AutocompleteAccept,
    AutocompleteNavigate(i32),
    KeyEvent(keyboard::Event),
    ToggleToolExpand(String),
    ToggleTool(String),
    ToggleClaudeProxy,
    UrlClicked(markdown::Uri),
    LoadSession(String),
    NewSession,
    DeleteSession(String),
    SwitchModel(String),
    // Settings
    OpenSettings,
    CloseSettings,
    SettingsTabChanged(SettingsTab),
    ProjectConfigAction(iced::widget::text_editor::Action),
    ModelsConfigAction(iced::widget::text_editor::Action),
    SaveProjectConfig,
    SaveModelsConfig,
    Noop,
}

impl RhoApp {
    pub fn new() -> (Self, IcedTask<Message>) {
        // Temporarily use the default Anthropic token for initial load;
        // will be properly resolved once model_config is built below.
        let initial_api_key = rho_core::auth::get_token().ok();
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

        let skill_dirs = rho_core::skills::default_skill_dirs(&cwd);
        let skills = rho_core::skills::discover_skills(&skill_dirs);

        let project_config = rho_core::config::load_project_config(&cwd);

        // Memories
        let memories = if project_config.memories {
            let memory_dirs = rho_core::memories::default_memory_dirs(&cwd);
            rho_core::memories::discover_memories(&memory_dirs)
        } else {
            Vec::new()
        };

        // Session store
        let session_db_path = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".rho")
            .join("sessions.db");
        let session_store = Arc::new(
            SessionStore::open(&session_db_path)
                .expect("Failed to open session database"),
        );
        let session_list = session_store.list_sessions(50).unwrap_or_default();

        let model_id = project_config
            .model
            .as_deref()
            .unwrap_or("claude-sonnet")
            .to_string();
        let thinking = project_config.thinking.unwrap_or(ThinkingLevel::Off);

        let model_registry = ModelRegistry::load();
        let model_config = model_registry
            .get(&model_id)
            .cloned()
            .unwrap_or_else(|| ModelConfig {
                id: model_id.clone(),
                provider: ProviderType::Anthropic,
                model_id: model_id.clone(),
                base_url: String::new(),
                api_key_env: Some("ANTHROPIC_API_KEY".into()),
                context_window: 200_000,
                max_tokens: if thinking != ThinkingLevel::Off { 16_384 } else { 8_192 },
                thinking: thinking != ThinkingLevel::Off,
                server_tools: None,
            });
        let model = ModelRegistry::to_model(&model_config);

        let event_handler_config = EventHandlerConfig {
            session_store: Some(session_store.clone()),
            session_id: None,
            model_id: model_id.to_string(),
            cwd: cwd.clone(),
        };

        let claude_proxy = Arc::new(AtomicBool::new(false));
        let available_tools: Vec<(Arc<dyn AgentTool>, bool)> = vec![
            (Arc::new(rho_tools::read::ReadTool::with_cwd(cwd.clone())), true),
            (Arc::new(rho_tools::write::WriteTool::with_cwd(cwd.clone())), true),
            (Arc::new(rho_tools::edit::EditTool::with_cwd(cwd.clone())), true),
            (Arc::new(rho_tools::bash::BashTool::new(cwd.clone())), true),
            (Arc::new(rho_tools::grep::GrepTool::new(cwd.clone())), true),
            (Arc::new(rho_tools::find::FindTool::new(cwd.clone())), true),
            (Arc::new(rho_tools::task::TaskTool::new(cwd.clone())), true),
            (Arc::new(rho_tools::web_fetch::WebFetchTool::with_claude_proxy(claude_proxy.clone())), true),
            (Arc::new(rho_tools::web_search::WebSearchTool::with_claude_proxy(claude_proxy.clone())), true),
        ];

        // Resolve API key for the configured model
        let api_key = ModelRegistry::resolve_api_key(&model_config)
            .ok()
            .or(initial_api_key);

        let app = Self {
            messages: Vec::new(),
            streaming_text: String::new(),
            streaming_markdown: markdown::Content::new(),
            input: String::new(),
            is_running: false,
            autocomplete: AutocompleteState::default(),
            skills,
            memories,
            conversation_history: Vec::new(),
            history: Vec::new(),
            history_index: None,
            history_draft: String::new(),
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
            model_config,
            model_registry,
            project_config,
            available_tools,
            claude_proxy,
            turn_tool_counts: HashMap::new(),
            session_store,
            current_session_id: None,
            session_list,
            event_handler_config,
            total_input_tokens: 0,
            total_output_tokens: 0,
            session_start: Instant::now(),
            settings_open: false,
            settings_tab: SettingsTab::Project,
            project_config_content: iced::widget::text_editor::Content::new(),
            models_config_content: iced::widget::text_editor::Content::new(),
        };

        (app, IcedTask::none())
    }

    pub fn update(&mut self, message: Message) -> IcedTask<Message> {
        match message {
            Message::InputChanged(text) => {
                // When in shell mode, the text_input displays without the `!` prefix.
                // Prepend it back so self.input always has the full `!command`.
                let was_shell_mode = self.input.starts_with('!');
                if was_shell_mode {
                    self.input = format!("!{}", text);
                } else {
                    self.input = text;
                }
                self.update_autocomplete();
                // Refocus input when transitioning into shell mode
                // (the text_input value switches from full input to stripped)
                if !was_shell_mode && self.input == "!" {
                    iced::widget::operation::focus(INPUT_ID)
                } else {
                    IcedTask::none()
                }
            }
            Message::SendPrompt => {
                if self.autocomplete.active {
                    return self.update(Message::AutocompleteAccept);
                }
                let task = self.handle_send_prompt();
                IcedTask::batch([task, iced::widget::operation::focus(INPUT_ID)])
            }
            Message::AutocompleteAccept => {
                if let Some(suggestion) = self.autocomplete.suggestions.get(self.autocomplete.selected).cloned() {
                    if let Some(ref trigger) = self.autocomplete.trigger {
                        let start = match trigger {
                            AutocompleteTrigger::Skill { start, .. } => *start,
                            AutocompleteTrigger::File { start, .. } => *start,
                        };
                        let mut new_input = self.input[..start].to_string();
                        new_input.push_str(&suggestion.completion);
                        if suggestion.is_directory {
                            // Keep popup open for directory navigation
                            self.input = new_input;
                            self.update_autocomplete();
                        } else {
                            new_input.push(' ');
                            self.input = new_input;
                            self.autocomplete.dismiss();
                        }
                    }
                }
                iced::widget::operation::move_cursor_to_end(INPUT_ID)
            }
            Message::AutocompleteNavigate(delta) => {
                self.autocomplete.navigate(delta);
                IcedTask::none()
            }
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
                // Inject into conversation history so the LLM sees shell results
                let truncated_output = truncate_shell_output(&result.output, 8000, 200);
                let history_text = if result.is_error {
                    format!("Shell command failed:\n$ {}\n\n{}", result.command, truncated_output)
                } else {
                    format!("Shell command output:\n$ {}\n\n{}", result.command, truncated_output)
                };
                self.conversation_history.push(rho_core::types::Message::User {
                    content: UserContent::Text(history_text),
                    timestamp: SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_millis() as u64,
                });

                self.messages.push(ConversationBlock::ShellOutput {
                    command: result.command,
                    output: result.output,
                    is_error: result.is_error,
                });
                self.is_running = false;
                IcedTask::none()
            }
            Message::KeyEvent(event) => {
                match event {
                    // Escape: dismiss autocomplete first, then cancel agent
                    keyboard::Event::KeyPressed {
                        key: keyboard::Key::Named(keyboard::key::Named::Escape),
                        ..
                    } => {
                        if self.autocomplete.active {
                            self.autocomplete.dismiss();
                        } else if self.is_running {
                            return self.update(Message::CancelAgent);
                        }
                    }
                    // Backspace: exit shell mode when command is empty
                    keyboard::Event::KeyPressed {
                        key: keyboard::Key::Named(keyboard::key::Named::Backspace),
                        ..
                    } if self.input == "!" => {
                        self.input.clear();
                        return iced::widget::operation::focus(INPUT_ID);
                    }
                    // Tab: accept autocomplete suggestion
                    keyboard::Event::KeyPressed {
                        key: keyboard::Key::Named(keyboard::key::Named::Tab),
                        ..
                    } if self.autocomplete.active => {
                        return self.update(Message::AutocompleteAccept);
                    }
                    // Up arrow: autocomplete navigation or history
                    keyboard::Event::KeyPressed {
                        key: keyboard::Key::Named(keyboard::key::Named::ArrowUp),
                        ..
                    } => {
                        if self.autocomplete.active {
                            return self.update(Message::AutocompleteNavigate(-1));
                        } else if !self.history.is_empty() {
                            self.history_up();
                            return iced::widget::operation::move_cursor_to_end(INPUT_ID);
                        }
                    }
                    // Down arrow: autocomplete navigation or history
                    keyboard::Event::KeyPressed {
                        key: keyboard::Key::Named(keyboard::key::Named::ArrowDown),
                        ..
                    } => {
                        if self.autocomplete.active {
                            return self.update(Message::AutocompleteNavigate(1));
                        } else if self.history_index.is_some() {
                            self.history_down();
                            return iced::widget::operation::move_cursor_to_end(INPUT_ID);
                        }
                    }
                    _ => {}
                }
                IcedTask::none()
            }
            Message::ToggleToolExpand(id) => {
                if !self.expanded_tools.remove(&id) {
                    self.expanded_tools.insert(id);
                }
                IcedTask::none()
            }
            Message::ToggleTool(name) => {
                for (tool, enabled) in &mut self.available_tools {
                    if tool.name() == name {
                        *enabled = !*enabled;
                        break;
                    }
                }
                IcedTask::none()
            }
            Message::ToggleClaudeProxy => {
                let prev = self.claude_proxy.load(Ordering::Relaxed);
                self.claude_proxy.store(!prev, Ordering::Relaxed);
                IcedTask::none()
            }
            Message::UrlClicked(_url) => {
                // Could open in browser; for now, no-op
                IcedTask::none()
            }
            Message::LoadSession(id) => {
                self.handle_load_session(&id);
                IcedTask::none()
            }
            Message::NewSession => {
                self.conversation_history.clear();
                self.messages.clear();
                self.current_session_id = None;
                self.total_input_tokens = 0;
                self.total_output_tokens = 0;
                self.session_start = Instant::now();
                IcedTask::none()
            }
            Message::DeleteSession(id) => {
                if let Ok(()) = self.session_store.delete_session(&id) {
                    self.session_list = self.session_store.list_sessions(50).unwrap_or_default();
                    if self.current_session_id.as_deref() == Some(&id) {
                        self.current_session_id = None;
                    }
                }
                IcedTask::none()
            }
            Message::SwitchModel(model_id) => {
                if let Some(config) = self.model_registry.get(&model_id).cloned() {
                    match ModelRegistry::resolve_api_key(&config) {
                        Ok(key) => {
                            self.model_config = config.clone();
                            self.model = ModelRegistry::to_model(&config);
                            self.api_key = Some(key);
                            self.error = None;
                            let provider = match config.provider {
                                ProviderType::Anthropic => "anthropic",
                                ProviderType::OpenAi => "openai",
                            };
                            let msg = format!("Switched to **{}** ({})", model_id, provider);
                            self.messages.push(ConversationBlock::AssistantMarkdown {
                                raw: msg.clone(),
                                items: markdown::Content::parse(&msg).items().to_vec(),
                            });
                        }
                        Err(e) => {
                            self.error =
                                Some(format!("Cannot switch to {}: {}", model_id, e));
                        }
                    }
                } else {
                    self.error = Some(format!("Unknown model: '{}'", model_id));
                }
                IcedTask::none()
            }
            Message::OpenSettings => {
                let project_text = self
                    .project_config
                    .source
                    .as_ref()
                    .and_then(|p| std::fs::read_to_string(p).ok())
                    .unwrap_or_else(|| {
                        "---\n# model: claude-sonnet\n# thinking: off\n# memories: true\n---\n\n\
                        # Project Instructions\n\nAdd your project-specific instructions here.\n"
                            .to_string()
                    });
                let models_text = dirs::home_dir()
                    .map(|h| h.join(".rho").join("models.toml"))
                    .and_then(|p| std::fs::read_to_string(p).ok())
                    .unwrap_or_else(|| {
                        "# Add custom models here.\n# Example:\n\
                        # [[model]]\n# id = \"gpt-4o\"\n# provider = \"openai\"\n\
                        # model_id = \"gpt-4o\"\n# api_key_env = \"OPENAI_API_KEY\"\n\
                        # context_window = 128000\n# max_tokens = 16384\n"
                            .to_string()
                    });
                self.project_config_content =
                    iced::widget::text_editor::Content::with_text(&project_text);
                self.models_config_content =
                    iced::widget::text_editor::Content::with_text(&models_text);
                self.settings_open = true;
                IcedTask::none()
            }
            Message::CloseSettings => {
                self.settings_open = false;
                IcedTask::none()
            }
            Message::SettingsTabChanged(tab) => {
                self.settings_tab = tab;
                IcedTask::none()
            }
            Message::ProjectConfigAction(action) => {
                self.project_config_content.perform(action);
                IcedTask::none()
            }
            Message::ModelsConfigAction(action) => {
                self.models_config_content.perform(action);
                IcedTask::none()
            }
            Message::SaveProjectConfig => {
                let text = self.project_config_content.text();
                let path = self
                    .project_config
                    .source
                    .clone()
                    .unwrap_or_else(|| self.cwd.join("RHO.md"));
                match std::fs::write(&path, &text) {
                    Ok(()) => {
                        self.project_config =
                            rho_core::config::load_project_config(&self.cwd);
                        self.error = None;
                    }
                    Err(e) => {
                        self.error = Some(format!("Failed to save {}: {}", path.display(), e));
                    }
                }
                IcedTask::none()
            }
            Message::SaveModelsConfig => {
                let text = self.models_config_content.text();
                if let Some(home) = dirs::home_dir() {
                    let dir = home.join(".rho");
                    let _ = std::fs::create_dir_all(&dir);
                    let path = dir.join("models.toml");
                    match std::fs::write(&path, &text) {
                        Ok(()) => {
                            self.model_registry = ModelRegistry::load();
                            self.error = None;
                        }
                        Err(e) => {
                            self.error = Some(format!("Failed to save models.toml: {}", e));
                        }
                    }
                }
                IcedTask::none()
            }
            Message::Noop => IcedTask::none(),
        }
    }

    fn update_autocomplete(&mut self) {
        match autocomplete::detect_trigger(&self.input) {
            Some(AutocompleteTrigger::Skill { ref query, .. }) => {
                let suggestions = autocomplete::list_skill_suggestions(&self.skills, &self.memories, query, &self.cwd);
                self.autocomplete.active = !suggestions.is_empty();
                self.autocomplete.trigger = autocomplete::detect_trigger(&self.input);
                self.autocomplete.suggestions = suggestions;
                self.autocomplete.selected = 0;
            }
            Some(AutocompleteTrigger::File { ref query, .. }) => {
                let suggestions = autocomplete::list_file_suggestions(&self.cwd, query);
                self.autocomplete.active = !suggestions.is_empty();
                self.autocomplete.trigger = autocomplete::detect_trigger(&self.input);
                self.autocomplete.suggestions = suggestions;
                self.autocomplete.selected = 0;
            }
            None => {
                self.autocomplete.dismiss();
            }
        }
    }

    fn history_up(&mut self) {
        match self.history_index {
            None => {
                // Save current input as draft, jump to most recent history entry
                self.history_draft = self.input.clone();
                self.history_index = Some(self.history.len() - 1);
                self.input = self.history[self.history.len() - 1].clone();
            }
            Some(idx) if idx > 0 => {
                self.history_index = Some(idx - 1);
                self.input = self.history[idx - 1].clone();
            }
            _ => {} // Already at oldest entry
        }
    }

    fn history_down(&mut self) {
        if let Some(idx) = self.history_index {
            if idx + 1 < self.history.len() {
                self.history_index = Some(idx + 1);
                self.input = self.history[idx + 1].clone();
            } else {
                // Back to draft
                self.history_index = None;
                self.input = std::mem::take(&mut self.history_draft);
            }
        }
    }

    fn handle_send_prompt(&mut self) -> IcedTask<Message> {
        if self.input.trim().is_empty() || self.is_running {
            return IcedTask::none();
        }

        let raw_input = self.input.clone();
        self.input.clear();
        self.autocomplete.dismiss();
        self.history_index = None;
        self.history_draft.clear();

        // Save to history (skip duplicates of last entry)
        if self.history.last().map_or(true, |last| last != &raw_input) {
            self.history.push(raw_input.clone());
        }

        // Handle /clear command
        if raw_input.trim() == "/clear" {
            self.conversation_history.clear();
            self.messages.clear();
            self.current_session_id = None;
            self.total_input_tokens = 0;
            self.total_output_tokens = 0;
            return IcedTask::none();
        }

        // Handle /compact command
        if raw_input.trim() == "/compact" {
            return self.handle_compact();
        }

        // Handle /model command
        if raw_input.trim() == "/model" || raw_input.trim().starts_with("/model ") {
            let args = raw_input.trim().strip_prefix("/model").unwrap_or("").trim();
            if args.is_empty() {
                // List all available models
                let model_list = self
                    .model_registry
                    .list()
                    .iter()
                    .map(|m| {
                        let provider = match m.provider {
                            ProviderType::Anthropic => "anthropic",
                            ProviderType::OpenAi => "openai",
                        };
                        let current = if m.id == self.model_config.id { " ◀ current" } else { "" };
                        format!("- **{}** ({}){}  ", m.id, provider, current)
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                self.messages.push(ConversationBlock::UserPrompt(
                    format!("/model"),
                ));
                let model_list_msg = format!("Available models:\n\n{}", model_list);
                self.messages.push(ConversationBlock::AssistantMarkdown {
                    raw: model_list_msg.clone(),
                    items: markdown::Content::parse(&model_list_msg).items().to_vec(),
                });
                return IcedTask::none();
            } else {
                return self.update(Message::SwitchModel(args.to_string()));
            }
        }

        let (display_text, resolved_text) =
            autocomplete::resolve_references(&raw_input, &self.cwd, &self.skills, &self.memories);

        // Check if this is a built-in command (e.g. /research, /plan, /implement)
        let final_text = if let Some(cmd_text) = resolved_text.strip_prefix('/') {
            let (cmd_name, cmd_args) = match cmd_text.split_once(' ') {
                Some((name, args)) => (name, args),
                None => (cmd_text, ""),
            };
            rho_core::commands::resolve_command(cmd_name, cmd_args, &self.cwd)
                .unwrap_or(resolved_text)
        } else {
            resolved_text
        };

        // Shell command (! prefix)
        if let Some(cmd) = display_text.strip_prefix('!') {
            let command = cmd.trim().to_string();
            if command.is_empty() {
                return IcedTask::none();
            }
            self.messages
                .push(ConversationBlock::UserPrompt(display_text.clone()));
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

        // Normal agent prompt — show original text in chat, send resolved to agent
        self.messages
            .push(ConversationBlock::UserPrompt(display_text.clone()));
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

        let mut system_prompt = self.project_config
            .system_prompt
            .clone()
            .unwrap_or_else(|| build_system_prompt_with_tools(&self.available_tools));

        // Add skills
        let skill_dirs = rho_core::skills::default_skill_dirs(&cwd);
        let skills = rho_core::skills::discover_skills(&skill_dirs);
        if !skills.is_empty() {
            system_prompt.push_str("\n\n");
            system_prompt.push_str(&rho_core::skills::format_skills_prompt(&skills));
        }

        // Add memories
        if !self.memories.is_empty() {
            system_prompt.push_str("\n\n");
            system_prompt.push_str(&rho_core::memories::format_memories_prompt(&self.memories));
        }

        // Add commands
        let commands = rho_core::commands::all_commands(&cwd);
        if !commands.is_empty() {
            system_prompt.push_str("\n\n<available_commands>\n");
            for cmd in &commands {
                system_prompt.push_str(&format!("  /{} — {}\n", cmd.name, cmd.description));
            }
            system_prompt.push_str("</available_commands>");
        }

        // Project config append
        if let Some(ref append) = self.project_config.system_prompt_append {
            system_prompt.push_str("\n\n");
            system_prompt.push_str(append);
        }

        let tools: Vec<Arc<dyn AgentTool>> = {
            let enabled: Vec<Arc<dyn AgentTool>> = self
                .available_tools
                .iter()
                .filter(|(_, enabled)| *enabled)
                .map(|(tool, _)| tool.clone())
                .collect();

            if let Some(ref allowed) = self.project_config.allowed_tools {
                enabled
                    .into_iter()
                    .filter(|t| allowed.iter().any(|a| a == t.name()))
                    .collect()
            } else {
                enabled
            }
        };

        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        // Multi-turn: pass accumulated history + new message
        let mut prompts = self.conversation_history.clone();
        prompts.push(rho_core::types::Message::User {
            content: UserContent::Text(final_text),
            timestamp: now_ms,
        });

        let thinking = self.project_config.thinking.unwrap_or(ThinkingLevel::Off);

        // Build compaction transform if configured
        let transform_messages = self.project_config.compact_threshold.map(|threshold| {
            compaction::make_compaction_transform(threshold)
        });

        let config = AgentLoopConfig {
            model: self.model.clone(),
            api_key,
            system_prompt,
            tools,
            thinking,
            max_tokens: None,
            stream_fn: rho_provider::stream_fn_for_model(&self.model_config),
            get_steering_messages: None,
            get_follow_up_messages: None,
            transform_messages,
            post_tools_hooks: vec![],
        };

        let consumer = agent_loop(prompts, config, cancel);
        let (task, handle) = IcedTask::run(consumer, Message::AgentEvent).abortable();
        self.abort_handle = Some(handle);
        task
    }

    fn handle_compact(&mut self) -> IcedTask<Message> {
        if self.conversation_history.is_empty() {
            return IcedTask::none();
        }

        // Prune tool outputs aggressively, keep last 4 messages
        self.conversation_history =
            compaction::prune_tool_outputs(&self.conversation_history, 4);

        self.messages
            .push(ConversationBlock::UserPrompt("/compact".into()));
        self.messages
            .push(ConversationBlock::AssistantMarkdown {
                raw: "Context compacted. Old tool outputs have been pruned.".into(),
                items: markdown::Content::parse("*Context compacted.* Old tool outputs have been pruned.")
                    .items()
                    .to_vec(),
            });

        IcedTask::none()
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
                tool_name,
                result,
                is_error,
                ..
            } => {
                *self.turn_tool_counts.entry(tool_name).or_insert(0) += 1;
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
            AgentEvent::AgentEnd { messages, .. } => {
                // Emit tool summary if any tools were used this turn
                if !self.turn_tool_counts.is_empty() {
                    let mut counts: Vec<(String, usize)> =
                        self.turn_tool_counts.drain().collect();
                    counts.sort_by(|a, b| b.1.cmp(&a.1));
                    self.messages
                        .push(ConversationBlock::ToolSummary(counts));
                }
                // Capture conversation history for multi-turn
                self.conversation_history = messages;
                self.is_running = false;
                self.abort_handle = None;

                // Session persistence
                self.persist_session();
            }
            AgentEvent::ContextCompacted { .. } => {
                // Could display a notification; for now, silent
            }
            _ => {}
        }
    }

    fn persist_session(&mut self) {
        // Sync current session ID into the event handler config
        self.event_handler_config.session_id = self.current_session_id.clone();

        let event = AgentEvent::AgentEnd {
            messages: self.conversation_history.clone(),
        };

        if let Some(result) = rho_core::event_handler::handle_event(
            &event,
            &mut self.event_handler_config,
        ) {
            self.apply_event_handler_result(result);
        }
    }

    fn apply_event_handler_result(&mut self, result: EventHandlerResult) {
        if let Some(id) = result.session_id {
            self.current_session_id = Some(id);
        }
        // Refresh sidebar list
        self.session_list = self.session_store.list_sessions(50).unwrap_or_default();
    }

    fn handle_load_session(&mut self, session_id: &str) {
        let messages = match self.session_store.load_messages(session_id) {
            Ok(m) => m,
            Err(_) => return,
        };

        self.conversation_history = messages;
        self.current_session_id = Some(session_id.to_string());
        self.total_input_tokens = 0;
        self.total_output_tokens = 0;
        self.session_start = Instant::now();

        // Rebuild conversation blocks from loaded messages
        self.messages.clear();
        for msg in &self.conversation_history {
            match msg {
                rho_core::types::Message::User {
                    content: UserContent::Text(text),
                    ..
                } => {
                    self.messages
                        .push(ConversationBlock::UserPrompt(text.clone()));
                }
                rho_core::types::Message::Assistant { content, usage, .. } => {
                    self.total_input_tokens += usage.input;
                    self.total_output_tokens += usage.output;
                    let text: String = content
                        .iter()
                        .filter_map(|c| match c {
                            Content::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("");
                    if !text.is_empty() {
                        let parsed = markdown::Content::parse(&text);
                        self.messages.push(ConversationBlock::AssistantMarkdown {
                            raw: text,
                            items: parsed.items().to_vec(),
                        });
                    }
                }
                _ => {
                    // ToolResult messages are part of history but not shown as blocks
                }
            }
        }
    }

    /// Estimate current context usage as a percentage.
    pub fn context_usage_percent(&self) -> f64 {
        if self.conversation_history.is_empty() {
            return 0.0;
        }
        let estimated = compaction::estimate_tokens(&self.conversation_history);
        let window = self.model.context_window;
        if window == 0 {
            return 0.0;
        }
        (estimated as f64 / window as f64 * 100.0).min(100.0)
    }
}

pub fn subscription(_app: &RhoApp) -> Subscription<Message> {
    // Use event::listen_with to see ALL keyboard events, even those
    // captured by text_input (e.g. Tab). Always active for history support.
    event::listen_with(|event, _status, _window| match event {
        iced::Event::Keyboard(kb_event) => Some(Message::KeyEvent(kb_event)),
        _ => None,
    })
}

/// Truncate large shell output, keeping head and tail lines.
///
/// If the output exceeds `max_chars`, keeps the first and last `tail_lines`
/// lines with a marker in between showing how much was omitted.
fn truncate_shell_output(output: &str, max_chars: usize, tail_lines: usize) -> String {
    if output.len() <= max_chars {
        return output.to_string();
    }

    let lines: Vec<&str> = output.lines().collect();
    if lines.len() <= tail_lines * 2 + 1 {
        // Too few lines to meaningfully split — just char-truncate
        let mut result = output[..max_chars].to_string();
        result.push_str("\n... [truncated]");
        return result;
    }

    // Take head lines until we approach half the budget
    let half = max_chars / 2;
    let mut head = String::new();
    let mut head_count = 0;
    for line in &lines {
        if head.len() + line.len() + 1 > half {
            break;
        }
        if !head.is_empty() {
            head.push('\n');
        }
        head.push_str(line);
        head_count += 1;
    }

    // Take tail lines from the end
    let tail_start = lines.len().saturating_sub(tail_lines);
    let tail: String = lines[tail_start..].join("\n");
    let omitted = lines.len() - head_count - (lines.len() - tail_start);

    format!(
        "{}\n\n... [{} lines omitted] ...\n\n{}",
        head, omitted, tail
    )
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

    #[test]
    fn clear_command_resets_state() {
        let (mut app, _) = RhoApp::new();
        app.conversation_history.push(rho_core::types::Message::User {
            content: UserContent::Text("test".into()),
            timestamp: 0,
        });
        app.messages.push(ConversationBlock::UserPrompt("test".into()));
        app.total_input_tokens = 100;
        app.total_output_tokens = 50;

        app.input = "/clear".into();
        let _ = app.update(Message::SendPrompt);

        assert!(app.conversation_history.is_empty());
        assert!(app.messages.is_empty());
        assert_eq!(app.total_input_tokens, 0);
        assert_eq!(app.total_output_tokens, 0);
    }

    #[test]
    fn agent_end_captures_history() {
        let (mut app, _) = RhoApp::new();
        let messages = vec![
            rho_core::types::Message::User {
                content: UserContent::Text("hello".into()),
                timestamp: 0,
            },
            rho_core::types::Message::Assistant {
                content: vec![Content::Text { text: "hi".into() }],
                model: "test".into(),
                usage: Usage::default(),
                stop_reason: StopReason::Stop,
                timestamp: 0,
            },
        ];
        app.is_running = true;
        app.handle_agent_event(AgentEvent::AgentEnd {
            messages: messages.clone(),
        });
        assert_eq!(app.conversation_history.len(), 2);
        assert!(!app.is_running);
    }

    #[test]
    fn context_usage_empty() {
        let (app, _) = RhoApp::new();
        assert_eq!(app.context_usage_percent(), 0.0);
    }

    #[test]
    fn truncate_shell_output_short() {
        let output = "line1\nline2\nline3";
        assert_eq!(truncate_shell_output(output, 8000, 200), output);
    }

    #[test]
    fn truncate_shell_output_large() {
        // Generate 500 lines
        let lines: Vec<String> = (0..500).map(|i| format!("line {}: some content here", i)).collect();
        let output = lines.join("\n");
        let truncated = truncate_shell_output(&output, 2000, 50);

        assert!(truncated.len() < output.len());
        assert!(truncated.contains("lines omitted"));
        // Should contain early lines
        assert!(truncated.contains("line 0:"));
        // Should contain late lines
        assert!(truncated.contains("line 499:"));
    }

    #[test]
    fn shell_done_injects_into_history() {
        let (mut app, _) = RhoApp::new();
        app.is_running = true;
        let _ = app.update(Message::ShellDone(ShellResult {
            command: "cargo test".into(),
            output: "all tests passed".into(),
            is_error: false,
        }));
        assert!(!app.is_running);
        assert_eq!(app.conversation_history.len(), 1);
        match &app.conversation_history[0] {
            rho_core::types::Message::User { content: UserContent::Text(t), .. } => {
                assert!(t.contains("cargo test"));
                assert!(t.contains("all tests passed"));
            }
            _ => panic!("expected User message"),
        }
    }
}
