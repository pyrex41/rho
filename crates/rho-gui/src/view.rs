use std::collections::HashSet;
use std::sync::atomic::Ordering;

use iced::widget::{
    button, column, container, markdown, row, scrollable, text, text_input, Column,
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
    let chat = render_chat(app);

    row![sidebar, chat].into()
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

    let mut col = Column::new().spacing(16).padding(16).width(220);

    // Project
    col = col
        .push(text("PROJECT").size(11).color(color!(0x565f89)))
        .push(text(dir_name).size(14));

    // Model
    col = col
        .push(text("MODEL").size(11).color(color!(0x565f89)))
        .push(text(app.model.name.as_str()).size(14));

    // Tokens
    col = col
        .push(text("TOKENS").size(11).color(color!(0x565f89)))
        .push(text(format!("In: {}", format_tokens(app.total_input_tokens))).size(13))
        .push(text(format!("Out: {}", format_tokens(app.total_output_tokens))).size(13));

    // Context
    let context_pct = app.context_usage_percent();
    if context_pct > 0.0 {
        let ctx_color = if context_pct > 80.0 {
            color!(0xf7768e)
        } else if context_pct > 50.0 {
            color!(0xe0af68)
        } else {
            color!(0xa9b1d6)
        };
        col = col
            .push(text("CONTEXT").size(11).color(color!(0x565f89)))
            .push(text(format!("~{:.0}% used", context_pct)).size(13).color(ctx_color));
    }

    // Turns
    let turns = app.conversation_history.iter()
        .filter(|m| matches!(m, rho_core::types::Message::User { .. }))
        .count();
    if turns > 0 {
        col = col
            .push(text("TURNS").size(11).color(color!(0x565f89)))
            .push(text(format!("{}", turns)).size(13));
    }

    // Session
    col = col
        .push(text("SESSION").size(11).color(color!(0x565f89)))
        .push(text(format!("{mins}m {secs:02}s")).size(13));

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

    container(col)
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

    for block in &app.messages {
        blocks_col = blocks_col.push(render_block(block, &app.expanded_tools));
    }

    // Live streaming markdown
    if !app.streaming_text.is_empty() {
        blocks_col = blocks_col.push(render_streaming_markdown(app));
    }

    let chat_area = scrollable(blocks_col).height(Length::Fill);

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
