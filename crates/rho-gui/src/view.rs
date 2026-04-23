use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::Ordering;

use iced::widget::{
    button, column, container, markdown, mouse_area, progress_bar, row, scrollable, text,
    text_editor, text_input, Column,
};
use iced::{color, Element, Font, Length, Theme};

use crate::app::{ConversationBlock, Message, RhoApp, ToolCallBlock};

// --- Fonts ---

pub const FONT_INTER: Font = Font::with_name("Inter");

pub const FONT_MONO: Font = Font {
    family: iced::font::Family::Name("JetBrains Mono"),
    weight: iced::font::Weight::Normal,
    stretch: iced::font::Stretch::Normal,
    style: iced::font::Style::Normal,
};

pub const FONT_MONO_BOLD: Font = Font {
    weight: iced::font::Weight::Bold,
    ..FONT_MONO
};

// --- Top-level layout ---

pub fn view(app: &RhoApp) -> Element<'_, Message> {
    let sidebar = render_sidebar(app);
    let main = if app.settings_open {
        render_settings(app)
    } else {
        render_chat(app)
    };

    // Drag handle for sidebar resize — wider hit area with visible center line
    let drag_handle = mouse_area(
        container(
            container(iced::widget::Space::new().width(2).height(Length::Fill))
                .style(|_theme: &Theme| container::Style {
                    background: Some(color!(0x565f89).into()),
                    ..Default::default()
                }),
        )
        .width(8)
        .height(Length::Fill)
        .center_x(8)
        .style(|_theme: &Theme| container::Style {
            background: Some(color!(0x1a1b26).into()),
            ..Default::default()
        }),
    )
    .on_press(Message::SidebarResizeStart)
    .interaction(iced::mouse::Interaction::ResizingColumn);

    row![sidebar, drag_handle, main].into()
}

pub fn theme(_app: &RhoApp) -> Theme {
    Theme::TokyoNight
}

// --- Sidebar (220px) ---

fn render_sidebar<'a>(app: &'a RhoApp) -> Element<'a, Message> {
    let dir_name = app
        .cwd
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(".");

    let elapsed = app.session_start.elapsed();
    let mins = elapsed.as_secs() / 60;
    let secs = elapsed.as_secs() % 60;

    let sidebar_w = app.sidebar_width;
    let mut col = Column::new().spacing(16).padding(16).width(sidebar_w);

    // Project + config button
    let cfg_label = if app.settings_open { "✕ Close" } else { "⚙ Config" };
    let cfg_btn = button(text(cfg_label).size(12).color(color!(0x7aa2f7)))
        .on_press(if app.settings_open {
            Message::CloseSettings
        } else {
            Message::OpenSettings
        })
        .padding([2, 6])
        .style(|_theme: &Theme, _status| button::Style {
            background: None,
            ..button::Style::default()
        });
    col = col
        .push(
            row![
                text("PROJECT").size(11).color(color!(0x565f89)),
                iced::widget::Space::new().width(Length::Fill),
                cfg_btn,
            ]
            .align_y(iced::Alignment::Center),
        )
        .push(text(dir_name).size(14));

    // Model picker — grouped by provider, collapsible
    col = col.push(text("MODELS").size(11).color(color!(0x565f89)));

    let all_models = app.model_registry.list();
    // Group models by API endpoint domain
    let mut by_provider: BTreeMap<String, Vec<&rho_core::models::ModelConfig>> = BTreeMap::new();
    for m in all_models {
        let label = model_group_label(m);
        by_provider.entry(label).or_default().push(m);
    }

    let max_id_len = (sidebar_w as usize).saturating_sub(60) / 7; // approx chars that fit

    for (provider, models) in &by_provider {
        let collapsed = app.collapsed_providers.contains(provider);
        let arrow = if collapsed { "▸" } else { "▾" };
        let avail_count = models.iter().filter(|m| app.available_model_ids.contains(&m.id)).count();
        let header_label = format!("{arrow} {provider}  ({avail_count}/{})", models.len());
        let provider_clone = provider.clone();
        let header_btn = button(
            text(header_label).size(11).color(color!(0x7aa2f7)),
        )
        .on_press(Message::ToggleProvider(provider_clone))
        .width(Length::Fill)
        .padding([2, 0])
        .style(|_theme, _status| button::Style {
            background: None,
            ..Default::default()
        });
        col = col.push(header_btn);

        if !collapsed {
            for m in models {
                let is_current = m.id == app.model_config.id;
                let available = app.available_model_ids.contains(&m.id);
                let label = if is_current {
                    format!("  ▶ {}", truncate_sidebar_text(&m.id, max_id_len))
                } else {
                    format!("    {}", truncate_sidebar_text(&m.id, max_id_len))
                };
                let color = if is_current {
                    color!(0x7aa2f7)
                } else if available {
                    color!(0xa9b1d6)
                } else {
                    color!(0x3b4261)
                };
                let model_id = m.id.clone();
                col = col.push(
                    button(text(label).size(12).color(color))
                        .style(|_theme, _status| button::Style {
                            background: None,
                            ..Default::default()
                        })
                        .on_press(Message::SwitchModel(model_id))
                        .padding([1, 0]),
                );
            }
        }
    }

    // Context usage — always visible
    let context_pct = app.context_usage_percent();
    let ctx_color = if context_pct > 80.0 {
        color!(0xf7768e)
    } else if context_pct > 50.0 {
        color!(0xe0af68)
    } else {
        color!(0x7aa2f7)
    };
    let turns = app.conversation_history.iter()
        .filter(|m| matches!(m, rho_core::types::Message::User { .. }))
        .count();
    let ctx_label = format!(
        "CONTEXT  {:.0}%  ({} turns, {}m {}s)",
        context_pct, turns, mins, secs
    );
    col = col
        .push(text(ctx_label).size(11).color(ctx_color))
        .push(
            progress_bar(0.0..=100.0, context_pct as f32)
                .style(move |_theme: &Theme| iced::widget::progress_bar::Style {
                    background: color!(0x1a1b2e).into(),
                    bar: ctx_color.into(),
                    border: iced::Border::default(),
                }),
        );

    // Tokens
    col = col
        .push(text("TOKENS").size(11).color(color!(0x565f89)))
        .push(
            row![
                text(format!("↑ {}", format_tokens(app.total_input_tokens))).size(12),
                text("  ").size(12),
                text(format!("↓ {}", format_tokens(app.total_output_tokens))).size(12),
            ]
        );

    // New chat button
    let new_btn = button(
        text("⟳ New Chat")
            .size(12)
            .font(FONT_MONO)
            .color(color!(0x7aa2f7)),
    )
    .on_press(Message::NewSession)
    .width(Length::Fill)
    .padding([4, 6])
    .style(move |_theme: &Theme, status| match status {
        button::Status::Hovered => button::Style {
            background: Some(color!(0x283457).into()),
            border: iced::Border {
                color: color!(0x7aa2f7),
                width: 1.0,
                radius: 4.0.into(),
            },
            ..button::Style::default()
        },
        _ => button::Style {
            background: Some(color!(0x1a1b2e).into()),
            border: iced::Border {
                color: color!(0x3b4261),
                width: 1.0,
                radius: 4.0.into(),
            },
            ..button::Style::default()
        },
    });
    col = col.push(new_btn);

    // Sessions
    col = col.push(text("SESSIONS").size(11).color(color!(0x565f89)));

    for session in app.session_list.iter().take(10) {
        let is_current = app.current_session_id.as_deref() == Some(&session.id);
        let title_color = if is_current {
            color!(0x7aa2f7)
        } else {
            color!(0xa9b1d6)
        };
        let session_id = session.id.clone();
        let delete_id = session.id.clone();

        let title_btn = button(
            text(truncate_sidebar_text(&session.title, 22))
                .size(12)
                .font(FONT_MONO)
                .color(title_color),
        )
        .on_press(Message::LoadSession(session_id))
        .width(Length::Fill)
        .padding([2, 4])
        .style(move |_theme: &Theme, status| match status {
            button::Status::Hovered => button::Style {
                background: Some(color!(0x283457).into()),
                ..button::Style::default()
            },
            _ => button::Style {
                background: if is_current {
                    Some(color!(0x1a1b26).into())
                } else {
                    None
                },
                ..button::Style::default()
            },
        });

        let del_btn = button(
            text("x")
                .size(10)
                .font(FONT_MONO)
                .color(color!(0x565f89)),
        )
        .on_press(Message::DeleteSession(delete_id))
        .padding([2, 4])
        .style(move |_theme: &Theme, status| match status {
            button::Status::Hovered => button::Style {
                background: Some(color!(0xf7768e).into()),
                ..button::Style::default()
            },
            _ => button::Style {
                background: None,
                ..button::Style::default()
            },
        });

        col = col.push(row![title_btn, del_btn].spacing(2));
    }

    // Memories
    if !app.memories.is_empty() {
        col = col.push(text("MEMORIES").size(11).color(color!(0x565f89)));
        for mem in &app.memories {
            let name = mem.name.clone();
            let btn = button(
                text(truncate_sidebar_text(&mem.name, 22))
                    .size(12)
                    .font(FONT_MONO)
                    .color(color!(0xbb9af7)),
            )
            .on_press(Message::InputChanged(format!("/{}", name)))
            .width(Length::Fill)
            .padding([2, 4])
            .style(move |_theme: &Theme, status| match status {
                button::Status::Hovered => button::Style {
                    background: Some(color!(0x283457).into()),
                    ..button::Style::default()
                },
                _ => button::Style {
                    background: None,
                    ..button::Style::default()
                },
            });
            col = col.push(btn);
        }
    }

    // Tools
    col = col.push(text("TOOLS").size(11).color(color!(0x565f89)));
    for (tool, enabled) in &app.available_tools {
        let name = tool.name().to_string();
        let label_color = if *enabled {
            color!(0x9ece6a)
        } else {
            color!(0x565f89)
        };
        let indicator = if *enabled { "on " } else { "off" };
        let label_text = format!("{indicator}  {name}");
        let btn = button(
            text(label_text)
                .size(12)
                .font(FONT_MONO)
                .color(label_color),
        )
        .on_press(Message::ToggleTool(name))
        .width(Length::Fill)
        .padding([2, 4])
        .style(move |_theme: &Theme, status| match status {
            button::Status::Hovered => button::Style {
                background: Some(color!(0x283457).into()),
                ..button::Style::default()
            },
            _ => button::Style {
                background: None,
                ..button::Style::default()
            },
        });
        col = col.push(btn);
    }

    // Claude proxy toggle
    let proxy_on = app.claude_proxy.load(Ordering::Relaxed);
    let proxy_color = if proxy_on {
        color!(0x7aa2f7)
    } else {
        color!(0x565f89)
    };
    let proxy_indicator = if proxy_on { "on " } else { "off" };
    let proxy_label = format!("{proxy_indicator}  claude proxy");
    let proxy_btn = button(
        text(proxy_label)
            .size(12)
            .font(FONT_MONO)
            .color(proxy_color),
    )
    .on_press(Message::ToggleClaudeProxy)
    .width(Length::Fill)
    .padding([2, 4])
    .style(move |_theme: &Theme, status| match status {
        button::Status::Hovered => button::Style {
            background: Some(color!(0x283457).into()),
            ..button::Style::default()
        },
        _ => button::Style {
            background: None,
            ..button::Style::default()
        },
    });
    col = col.push(proxy_btn);

    // Error
    if let Some(ref err) = app.error {
        col = col
            .push(text("ERROR").size(11).color(color!(0xf7768e)))
            .push(text(err.as_str()).size(12).color(color!(0xf7768e)));
    }

    container(scrollable(col).height(Length::Fill))
        .width(sidebar_w)
        .height(Length::Fill)
        .style(|theme: &Theme| {
            let palette = theme.extended_palette();
            container::Style {
                background: Some(palette.background.weak.color.into()),
                ..Default::default()
            }
        })
        .into()
}

fn model_group_label(m: &rho_core::models::ModelConfig) -> String {
    if m.base_url.is_empty() {
        // No base_url means provider default
        return match m.provider {
            rho_core::models::ProviderType::Anthropic => "Anthropic".into(),
            rho_core::models::ProviderType::OpenAi => "OpenAI".into(),
            rho_core::models::ProviderType::XaiResponses => "xAI".into(),
            rho_core::models::ProviderType::LlamaCpp => "llama.cpp".into(),
        };
    }
    // Group by domain from base_url
    if let Some(host) = m.base_url.split("//").nth(1).and_then(|s| s.split('/').next()) {
        match host {
            "api.anthropic.com" => "Anthropic".into(),
            "api.openai.com" => "OpenAI".into(),
            "api.x.ai" => "xAI".into(),
            _ => host.to_string(),
        }
    } else {
        "Other".into()
    }
}

fn truncate_sidebar_text(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max.saturating_sub(3)])
    }
}

fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

// --- Chat area ---

fn render_chat<'a>(app: &'a RhoApp) -> Element<'a, Message> {
    let mut blocks_col = Column::new().spacing(12).padding(20);

    // Show welcome/setup screen if no models are available and chat is empty
    if app.available_model_ids.is_empty() && app.messages.is_empty() {
        blocks_col = blocks_col.push(render_setup_guide());
    }

    for block in &app.messages {
        blocks_col = blocks_col.push(render_block(block, &app.expanded_tools));
    }

    // Live streaming markdown
    if !app.streaming_text.is_empty() {
        blocks_col = blocks_col.push(render_streaming_markdown(app));
    }

    let chat_area = scrollable(blocks_col)
        .height(Length::Fill)
        .anchor_bottom();

    // Autocomplete popup
    let autocomplete_popup: Element<'_, Message> = if app.autocomplete.active {
        let mut col = Column::new().spacing(0);
        for (i, suggestion) in app.autocomplete.suggestions.iter().enumerate() {
            let is_selected = i == app.autocomplete.selected;
            let label = text(&suggestion.display)
                .size(13)
                .font(FONT_MONO)
                .color(if is_selected {
                    color!(0x7aa2f7)
                } else {
                    color!(0xa9b1d6)
                });
            let btn = button(label)
                .on_press(Message::AutocompleteAccept)
                .width(Length::Fill)
                .padding([4, 8])
                .style(move |_theme: &Theme, _status| {
                    if is_selected {
                        button::Style {
                            background: Some(color!(0x283457).into()),
                            ..button::Style::default()
                        }
                    } else {
                        button::Style {
                            background: None,
                            ..button::Style::default()
                        }
                    }
                });
            col = col.push(btn);
        }
        container(col)
            .width(Length::Fill)
            .style(|theme: &Theme| {
                let palette = theme.extended_palette();
                container::Style {
                    background: Some(palette.background.strong.color.into()),
                    border: iced::Border {
                        radius: 4.0.into(),
                        width: 1.0,
                        color: color!(0x565f89),
                    },
                    ..Default::default()
                }
            })
            .padding([4, 0])
            .into()
    } else {
        column![].into()
    };

    let shell_mode = app.input.starts_with('!');
    let input_display: &str = if shell_mode {
        &app.input[1..]
    } else {
        &app.input
    };

    let placeholder = if app.is_running {
        "Agent working... (Esc to cancel)"
    } else if shell_mode {
        "Enter shell command..."
    } else {
        "Type a message... (! for shell)"
    };

    let send_label = if app.is_running {
        "..."
    } else if shell_mode {
        "Run"
    } else {
        "Send"
    };
    let send_button = if app.is_running {
        button(send_label).padding([12, 24])
    } else {
        button(send_label)
            .on_press(Message::SendPrompt)
            .padding([12, 24])
    };

    let input_field = text_input(placeholder, input_display)
        .id(crate::app::INPUT_ID)
        .on_input(Message::InputChanged)
        .on_submit(Message::SendPrompt)
        .padding(12);

    let input_area: Element<'_, Message> = if shell_mode {
        let styled_input = input_field.style(|theme: &Theme, status| {
            let mut s = text_input::default(theme, status);
            s.border.color = color!(0xe0af68);
            s.border.width = 2.0;
            s
        });
        let prefix = text("!")
            .size(18)
            .font(FONT_MONO_BOLD)
            .color(color!(0xe0af68));
        row![prefix, styled_input, send_button]
            .spacing(8)
            .padding(16)
            .align_y(iced::Alignment::Center)
            .into()
    } else {
        row![input_field, send_button]
            .spacing(8)
            .padding(16)
            .into()
    };

    container(column![chat_area, autocomplete_popup, input_area])
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

// --- Setup guide (first-run) ---

fn render_setup_guide<'a>() -> Element<'a, Message> {
    let heading = text("Welcome to Rho")
        .size(22)
        .font(FONT_INTER)
        .color(color!(0x7aa2f7));

    let subtitle = text("No API keys detected. Configure a provider to get started:")
        .size(14)
        .font(FONT_INTER)
        .color(color!(0xa9b1d6));

    let hint = text("After setting an env var, restart Rho. Models with valid keys appear highlighted in the sidebar.")
        .size(12)
        .font(FONT_INTER)
        .color(color!(0x565f89));

    container(
        column![
            heading,
            subtitle,
            setup_section(
                "Anthropic (Claude)",
                "export ANTHROPIC_API_KEY=sk-ant-...\n\nOr log in with Claude Code \u{2014} Rho reads its OAuth credentials automatically.",
            ),
            setup_section(
                "OpenAI (GPT)",
                "export OPENAI_API_KEY=sk-...",
            ),
            setup_section(
                "xAI (Grok)",
                "export XAI_API_KEY=xai-...",
            ),
            setup_section(
                "Ollama (local, no key needed)",
                "1. Install Ollama and pull a model:  ollama pull gemma2:9b\n2. Click Config in the sidebar, go to Models tab, and add:\n\n[[model]]\nid = \"ollama-gemma\"\nprovider = \"openai\"\nmodel_id = \"gemma2:9b\"\nbase_url = \"http://localhost:11434/v1\"\ncontext_window = 8192\nmax_tokens = 4096",
            ),
            hint,
        ]
        .spacing(12)
        .width(Length::Fill),
    )
    .padding(8)
    .width(Length::Fill)
    .into()
}

fn setup_section<'a>(title: &'a str, body: &'a str) -> Element<'a, Message> {
    container(
        column![
            text(title)
                .size(14)
                .font(FONT_MONO_BOLD)
                .color(color!(0x9ece6a)),
            text(body)
                .size(13)
                .font(FONT_MONO)
                .color(color!(0xa9b1d6)),
        ]
        .spacing(6),
    )
    .padding(12)
    .width(Length::Fill)
    .style(|theme: &Theme| {
        let palette = theme.extended_palette();
        container::Style {
            background: Some(palette.background.strong.color.into()),
            border: iced::Border {
                radius: 6.0.into(),
                ..Default::default()
            },
            ..Default::default()
        }
    })
    .into()
}

// --- Block rendering ---

fn render_block<'a>(
    block: &'a ConversationBlock,
    expanded: &'a HashSet<String>,
) -> Element<'a, Message> {
    match block {
        ConversationBlock::UserPrompt(content) => render_user_prompt(content),
        ConversationBlock::AssistantMarkdown { items, .. } => render_assistant_markdown(items),
        ConversationBlock::ShellOutput {
            command,
            output,
            is_error,
        } => render_shell_output(command, output, *is_error),
        ConversationBlock::ToolCall(tc) => render_tool_call(tc, expanded),
        ConversationBlock::ToolSummary(counts) => render_tool_summary(counts),
    }
}

fn render_user_prompt(content: &str) -> Element<'_, Message> {
    container(
        text(content)
            .size(14)
            .color(color!(0x7aa2f7))
            .font(FONT_INTER),
    )
    .padding(12)
    .width(Length::Fill)
    .style(|theme: &Theme| {
        let palette = theme.extended_palette();
        container::Style {
            background: Some(palette.background.weak.color.into()),
            border: iced::Border {
                radius: 6.0.into(),
                ..Default::default()
            },
            ..Default::default()
        }
    })
    .into()
}

fn render_assistant_markdown<'a>(items: &'a [markdown::Item]) -> Element<'a, Message> {
    let settings = markdown::Settings {
        text_size: 14.0.into(),
        h1_size: 24.0.into(),
        h2_size: 20.0.into(),
        h3_size: 17.0.into(),
        h4_size: 15.0.into(),
        h5_size: 14.0.into(),
        h6_size: 13.0.into(),
        code_size: 13.0.into(),
        ..markdown::Settings::with_text_size(14, &Theme::TokyoNight)
    };

    container(markdown::view(items, settings).map(Message::UrlClicked))
        .padding(12)
        .width(Length::Fill)
        .into()
}

fn render_streaming_markdown(app: &RhoApp) -> Element<'_, Message> {
    let items = app.streaming_markdown.items();

    if items.is_empty() {
        // Fallback to plain text while markdown hasn't parsed yet
        return container(text(&app.streaming_text).size(14))
            .padding(12)
            .width(Length::Fill)
            .into();
    }

    let settings = markdown::Settings {
        text_size: 14.0.into(),
        h1_size: 24.0.into(),
        h2_size: 20.0.into(),
        h3_size: 17.0.into(),
        h4_size: 15.0.into(),
        h5_size: 14.0.into(),
        h6_size: 13.0.into(),
        code_size: 13.0.into(),
        ..markdown::Settings::with_text_size(14, &Theme::TokyoNight)
    };

    container(markdown::view(items, settings).map(Message::UrlClicked))
        .padding(12)
        .width(Length::Fill)
        .into()
}

fn render_shell_output<'a>(
    command: &'a str,
    output: &'a str,
    is_error: bool,
) -> Element<'a, Message> {
    let header_color = if is_error {
        color!(0xf7768e)
    } else {
        color!(0x9ece6a)
    };

    let col = column![
        text(format!("$ {command}")).size(13).font(FONT_MONO).color(header_color),
        text(output)
            .size(12)
            .font(FONT_MONO)
            .color(color!(0xa9b1d6)),
    ]
    .spacing(4);

    container(col)
        .padding(12)
        .width(Length::Fill)
        .style(|theme: &Theme| {
            let palette = theme.extended_palette();
            container::Style {
                background: Some(palette.background.strong.color.into()),
                border: iced::Border {
                    radius: 6.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            }
        })
        .into()
}

fn render_tool_call<'a>(
    tc: &'a ToolCallBlock,
    expanded: &'a HashSet<String>,
) -> Element<'a, Message> {
    let is_expanded = expanded.contains(&tc.id);

    let arrow = if is_expanded { "v " } else { "> " };
    let status = if tc.result.is_some() {
        if tc.is_error {
            " ERR"
        } else {
            " ok"
        }
    } else {
        " ..."
    };
    let label = format!("{}{}{}", arrow, tc.name, status);

    let status_color = if tc.is_error {
        color!(0xf7768e)
    } else {
        color!(0x9ece6a)
    };

    let header = button(text(label).size(13).color(status_color))
        .on_press(Message::ToggleToolExpand(tc.id.clone()))
        .style(|theme: &Theme, status| {
            let palette = theme.extended_palette();
            match status {
                button::Status::Hovered => button::Style {
                    background: Some(palette.background.weak.color.into()),
                    ..button::Style::default()
                },
                _ => button::Style {
                    background: None,
                    ..button::Style::default()
                },
            }
        });

    let mut col = Column::new().spacing(4).push(header);
    if is_expanded {
        col = col.push(
            text(tc.args.as_str())
                .size(11)
                .font(FONT_MONO)
                .color(color!(0x565f89)),
        );
        if let Some(ref result) = tc.result {
            let result_color = if tc.is_error {
                color!(0xf7768e)
            } else {
                color!(0xa9b1d6)
            };
            // Truncate long results for display
            let display = if result.len() > 2000 {
                format!("{}...", &result[..2000])
            } else {
                result.clone()
            };
            col = col.push(
                text(display)
                    .size(11)
                    .font(FONT_MONO)
                    .color(result_color),
            );
        }
    }

    container(col).padding(8).width(Length::Fill).into()
}

fn render_tool_summary(counts: &[(String, usize)]) -> Element<'_, Message> {
    let summary = counts
        .iter()
        .map(|(name, count)| {
            if *count > 1 {
                format!("{name} x{count}")
            } else {
                name.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(", ");

    container(
        text(format!("> {summary}"))
            .size(12)
            .font(FONT_MONO)
            .color(color!(0x565f89)),
    )
    .padding([4, 12])
    .width(Length::Fill)
    .into()
}

// --- Settings panel ---

fn render_settings(app: &RhoApp) -> Element<'_, Message> {
    use crate::app::SettingsTab;

    let tab_btn = |label: String, tab: SettingsTab, active: bool| {
        let color = if active { color!(0x7aa2f7) } else { color!(0x565f89) };
        button(text(label).size(13).color(color))
            .on_press(Message::SettingsTabChanged(tab))
            .padding([4, 12])
            .style(move |_theme: &Theme, _status| button::Style {
                background: if active {
                    Some(color!(0x1a1b2e).into())
                } else {
                    None
                },
                border: if active {
                    iced::Border {
                        color: color!(0x7aa2f7),
                        width: 1.0,
                        radius: 4.0.into(),
                    }
                } else {
                    iced::Border::default()
                },
                ..button::Style::default()
            })
    };

    let tabs = row![
        tab_btn(
            "Project (RHO.md)".into(),
            SettingsTab::Project,
            app.settings_tab == SettingsTab::Project
        ),
        tab_btn(
            "Models (~/.rho/models.toml)".into(),
            SettingsTab::Models,
            app.settings_tab == SettingsTab::Models
        ),
    ]
    .spacing(8);

    let (editor_elem, file_label, save_msg): (Element<'_, Message>, String, Message) = match app.settings_tab {
        SettingsTab::Project => {
            let path_label = app
                .project_config
                .source
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| format!("{}/RHO.md (will be created)", app.cwd.display()));
            let ed = text_editor(&app.project_config_content)
                .on_action(Message::ProjectConfigAction)
                .font(FONT_MONO)
                .height(Length::Fill)
                .style(|_theme: &Theme, _status| text_editor::Style {
                    background: color!(0x0d0e17).into(),
                    border: iced::Border {
                        color: color!(0x3b4261),
                        width: 1.0,
                        radius: 4.0.into(),
                    },
                    placeholder: color!(0x565f89),
                    value: color!(0xa9b1d6),
                    selection: color!(0x283457),
                });
            (ed.into(), path_label, Message::SaveProjectConfig)
        }
        SettingsTab::Models => {
            let path_label = dirs::home_dir()
                .map(|h| h.join(".rho").join("models.toml").display().to_string())
                .unwrap_or_else(|| "~/.rho/models.toml".to_string());
            let ed = text_editor(&app.models_config_content)
                .on_action(Message::ModelsConfigAction)
                .font(FONT_MONO)
                .height(Length::Fill)
                .style(|_theme: &Theme, _status| text_editor::Style {
                    background: color!(0x0d0e17).into(),
                    border: iced::Border {
                        color: color!(0x3b4261),
                        width: 1.0,
                        radius: 4.0.into(),
                    },
                    placeholder: color!(0x565f89),
                    value: color!(0xa9b1d6),
                    selection: color!(0x283457),
                });
            (ed.into(), path_label, Message::SaveModelsConfig)
        }
    };

    let save_btn = button(text("Save").size(13).color(color!(0x9ece6a)))
        .on_press(save_msg)
        .padding([6, 16])
        .style(|_theme: &Theme, status| button::Style {
            background: Some(match status {
                button::Status::Hovered => color!(0x1a2e1a).into(),
                _ => color!(0x0d1a0d).into(),
            }),
            border: iced::Border {
                color: color!(0x9ece6a),
                width: 1.0,
                radius: 4.0.into(),
            },
            ..button::Style::default()
        });

    let toolbar = row![
        text(file_label).size(11).color(color!(0x565f89)),
        iced::widget::Space::new().width(Length::Fill),
        save_btn,
    ]
    .align_y(iced::Alignment::Center);

    container(
        column![tabs, toolbar, editor_elem]
            .spacing(12)
            .padding(20)
            .width(Length::Fill)
            .height(Length::Fill),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}
