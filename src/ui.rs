use crate::{
    app::{App, Prompt, View},
    grouping::Row,
    model::{folder_display_name, safe_text},
    theme::{self, Theme},
};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
};
use unicode_width::UnicodeWidthStr;

fn panel(theme: &Theme, title: impl Into<Line<'static>>, active: bool) -> Block<'static> {
    Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(theme.style(if active {
            "border_active"
        } else {
            "border_inactive"
        }))
        .style(theme.style("panel"))
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    frame.render_widget(Block::default().style(app.theme.style("background")), area);
    if area.width < 40 || area.height < 12 {
        frame.render_widget(Paragraph::new("TerminalEmail\n\nPlease resize to at least 40 × 12.\nNavigation and quit keys remain active.").style(app.theme.style("accent")), area);
        return;
    }
    let regions = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(5),
        Constraint::Length(2),
    ])
    .split(area);
    let label = if app.demo {
        "DEMO"
    } else if app.offline {
        "OFFLINE"
    } else {
        "MAIL"
    };
    let title = Line::from(vec![
        Span::styled("  TERMINAL / EMAIL", app.theme.style("accent").bold()),
        Span::styled(
            format!("    {label}    {}", app.config.account.email),
            app.theme.style("muted"),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(title).block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(app.theme.style("muted")),
        ),
        regions[0],
    );
    app.page_height = regions[1].height.saturating_sub(2) as usize;
    if regions[1].width >= 90 {
        let columns =
            Layout::horizontal([Constraint::Length(25), Constraint::Min(40)]).split(regions[1]);
        folders(frame, app, columns[0]);
        content(frame, app, columns[1]);
    } else if app.view == View::Folders {
        folders(frame, app, regions[1]);
    } else {
        content(frame, app, regions[1]);
    }
    let mode = if app.theme_picker.is_some() {
        "THEMES"
    } else if app.prompt.is_some() {
        "INPUT"
    } else {
        match app.view {
            View::Folders => "FOLDERS",
            View::Threads => "THREADS",
            View::Message => "MESSAGE",
        }
    };
    let status = safe_text(&app.status).replace('\n', " ");
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled(format!(" {mode} "), app.theme.style("mode")),
                Span::styled(
                    format!("  {status}"),
                    app.theme.style(if app.error { "error" } else { "text" }),
                ),
            ]),
            Line::styled(
                app.theme_warning.as_ref().map(|warning| format!(" Space t themes · {}", safe_text(warning).replace('\n', " "))).unwrap_or_else(||
                    " j/k move  l open/fold  h back  t time groups  Space t themes  / search  c compose  ? help".into()),
                app.theme.style(if app.theme_warning.is_some() { "error" } else { "muted" }),
            ),
        ]),
        regions[2],
    );
    if app.help {
        help(frame, app);
    }
    if let Some(prompt) = &app.prompt {
        draw_prompt(frame, prompt, &app.theme);
    }
    if app.theme_picker.is_some() {
        draw_theme_picker(frame, app);
    }
}

fn folders(frame: &mut Frame, app: &App, area: Rect) {
    let items: Vec<_> = app
        .folders
        .iter()
        .map(|folder| {
            let name = folder_display_name(&folder.name);
            let count = if folder.unread > 0 {
                format!(" ({})", folder.unread)
            } else {
                String::new()
            };
            ListItem::new(Line::from(vec![
                Span::styled(name, app.theme.style("text")),
                Span::styled(count, app.theme.style("starred")),
            ]))
        })
        .collect();
    if items.is_empty() {
        frame.render_widget(
            Paragraph::new("No cached folders.\n\nConnecting…\n\nCtrl-r to retry\n? for help")
                .block(panel(&app.theme, " Folders ", app.view == View::Folders)),
            area,
        );
    } else {
        let list = List::new(items)
            .block(panel(&app.theme, " Folders ", app.view == View::Folders))
            .highlight_style(app.theme.style("folder_selection"))
            .highlight_symbol("› ");
        frame.render_stateful_widget(
            list,
            area,
            &mut ListState::default().with_selected(Some(app.folder_cursor)),
        );
    }
}

fn content(frame: &mut Frame, app: &mut App, area: Rect) {
    if app.view == View::Message {
        if area.height >= 23 {
            let parts = Layout::vertical([Constraint::Length(7), Constraint::Min(10)]).split(area);
            message_list(frame, app, parts[0]);
            reader(frame, app, parts[1]);
        } else {
            reader(frame, app, area);
        }
    } else {
        message_list(frame, app, area);
    }
}

fn message_list(frame: &mut Frame, app: &App, area: Rect) {
    let title = if app.search.is_empty() {
        format!(
            " {} · {} conversations ",
            folder_display_name(&app.active_folder),
            app.groups.len()
        )
    } else {
        format!(
            " Local search: {} · {} conversations ",
            safe_text(&app.search),
            app.groups.len()
        )
    };
    let title = format!(
        "{} · time grouping {} ",
        title.trim_end(),
        if app.time_grouping { "on" } else { "off" }
    );
    let block = panel(&app.theme, title, app.view == View::Threads);
    if app.groups.is_empty() {
        frame.render_widget(Paragraph::new("\n  No messages to show.\n\n  Ctrl-r refreshes the mailbox.\n  Esc clears a search.\n  c starts a new message.").style(app.theme.style("muted")).block(block), area);
        return;
    }
    let items = app
        .rows
        .iter()
        .map(|row| {
            let group = match row {
                Row::Heading(period, count) => {
                    return ListItem::new(Line::styled(
                        format!(
                            "{} {} · {} conversations",
                            if app.collapsed.contains(period) {
                                "▸"
                            } else {
                                "▾"
                            },
                            period.label(),
                            count
                        ),
                        app.theme.style("time_heading"),
                    ));
                }
                Row::Conversation(group) => &app.groups[*group],
            };
            let message = &app.messages[*group.last().unwrap()];
            let unread = group.iter().any(|&i| !app.messages[i].seen);
            let starred = group.iter().any(|&i| app.messages[i].starred);
            let marker = if unread { "●" } else { "·" };
            let count = if group.len() > 1 {
                format!("  [{}]", group.len())
            } else {
                String::new()
            };
            let style = if unread {
                app.theme.style("unread")
            } else {
                app.theme.style("read")
            };
            ListItem::new(vec![
                Line::from(vec![
                    Span::styled(
                        format!("{marker} {} ", if starred { "★" } else { " " }),
                        app.theme.style(if starred { "starred" } else { "accent" }),
                    ),
                    Span::styled(safe_text(&message.subject), style),
                    Span::styled(count, app.theme.style("accent")),
                ]),
                Line::styled(
                    format!(
                        "     {}   {}",
                        safe_text(&message.from),
                        safe_text(&message.date)
                    ),
                    app.theme.style("muted"),
                ),
            ])
        })
        .collect::<Vec<_>>();
    frame.render_stateful_widget(
        List::new(items)
            .block(block)
            .highlight_symbol("› ")
            .highlight_style(app.theme.style("conversation_selection")),
        area,
        &mut ListState::default().with_selected(Some(app.list_cursor)),
    );
}

fn reader(frame: &mut Frame, app: &mut App, area: Rect) {
    let count = app.groups.get(app.thread_cursor).map_or(0, Vec::len);
    let block = panel(
        &app.theme,
        format!(
            " Message {}/{} · [ previous / ] next ",
            app.message_cursor + 1,
            count
        ),
        true,
    );
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let Some(message) = app.selected() else {
        return;
    };
    let notice = app.body_notice(message);
    let mut text = format!(
        "{}\nFrom: {}\nTo: {}\nDate: {}\n\n{}",
        message.subject,
        message.from,
        message.to,
        message.date,
        message.body.as_deref().unwrap_or(&notice)
    );
    if !message.attachments.is_empty() {
        text.push_str("\n\nAttachments (S to save):\n");
        for (i, attachment) in message.attachments.iter().enumerate() {
            text.push_str(&format!(
                "  {}. {} · {} bytes\n",
                i + 1,
                attachment.name,
                attachment.data.len()
            ));
        }
    }
    let lines: Vec<Line<'static>> = safe_text(&text)
        .lines()
        .flat_map(|line| {
            if line.is_empty() {
                vec![Line::default()]
            } else {
                textwrap::wrap(line, inner.width.max(1) as usize)
                    .into_iter()
                    .map(|s| Line::raw(s.into_owned()))
                    .collect()
            }
        })
        .collect();
    let max_scroll = lines
        .len()
        .saturating_sub(inner.height as usize)
        .min(u16::MAX as usize) as u16;
    app.scroll = app.scroll.min(max_scroll);
    app.page_height = inner.height as usize;
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .style(app.theme.style("text"))
            .scroll((app.scroll, 0)),
        inner,
    );
}

fn popup(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width.saturating_sub(4));
    let height = height.min(area.height.saturating_sub(2));
    Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    )
}

fn help(frame: &mut Frame, app: &App) {
    let area = popup(frame.area(), 85, frame.area().height.saturating_sub(4));
    let mut lines = vec![
        Line::styled(
            "  Vim navigation · TOML configurable",
            app.theme.style("accent").bold(),
        ),
        Line::styled(
            "  Navigation bindings scroll · back or help closes",
            app.theme.style("text"),
        ),
        Line::default(),
    ];
    for (context, bindings) in &app.keys.bindings {
        lines.push(Line::styled(
            format!("  [{context}]"),
            app.theme.style("starred"),
        ));
        for (key, command) in bindings {
            lines.push(Line::styled(
                format!("  {key:18} {command}"),
                app.theme.style("text"),
            ));
        }
        lines.push(Line::default());
    }
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines)
            .scroll((app.help_scroll, 0))
            .block(panel(&app.theme, " Configured bindings ", true)),
        area,
    );
}

fn draw_theme_picker(frame: &mut Frame, app: &App) {
    let picker = app.theme_picker.as_ref().unwrap();
    let area = popup(frame.area(), 86, 20);
    frame.render_widget(Clear, area);
    let title = format!(
        " Themes · {}/{} · current: {} ",
        picker.matches.len(),
        picker.catalog.entries.len(),
        safe_text(&picker.original_id)
    );
    let block = panel(&app.theme, title, true);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let parts = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(2),
        Constraint::Length(1),
    ])
    .split(inner);
    let sanitized = safe_text(&picker.query);
    let mut input = sanitized.as_str();
    while input.width() > parts[0].width.saturating_sub(3) as usize {
        input = &input[input.chars().next().map_or(0, char::len_utf8)..];
    }
    frame.render_widget(
        Paragraph::new(format!("> {input}")).style(app.theme.style("prompt_input")),
        parts[0],
    );
    if parts[0].width > 2 && parts[0].height > 0 {
        frame.set_cursor_position((parts[0].x + 2 + input.width() as u16, parts[0].y));
    }
    let items: Vec<_> = picker
        .matches
        .iter()
        .map(|&i| {
            let entry = &picker.catalog.entries[i];
            let label = safe_text(&format!("{} ({})", entry.name, entry.id));
            let positions = theme::fuzzy(&picker.query, &label)
                .map(|m| m.1)
                .unwrap_or_default();
            let normal = app.theme.style(if entry.theme.is_some() {
                "text"
            } else {
                "error"
            });
            let mut spans = vec![Span::styled(
                if entry.id == picker.original_id && entry.theme.is_some() {
                    "● "
                } else {
                    "  "
                },
                normal,
            )];
            spans.extend(label.chars().enumerate().map(|(i, ch)| {
                Span::styled(
                    ch.to_string(),
                    if positions.contains(&i) {
                        normal.patch(app.theme.style("picker_match"))
                    } else {
                        normal
                    },
                )
            }));
            ListItem::new(Line::from(spans))
        })
        .collect();
    if items.is_empty() {
        frame.render_widget(
            Paragraph::new("No matching themes").style(app.theme.style("muted")),
            parts[1],
        );
    } else {
        frame.render_stateful_widget(
            List::new(items)
                .highlight_symbol("› ")
                .highlight_style(app.theme.style("picker_selection")),
            parts[1],
            &mut ListState::default().with_selected(Some(picker.selected)),
        );
    }
    let diagnostic = picker
        .error
        .as_deref()
        .or_else(|| picker.entry().and_then(|e| e.error.as_deref()))
        .or_else(|| picker.catalog.diagnostics.first().map(String::as_str));
    let message = diagnostic
        .map(safe_text)
        .unwrap_or_else(|| "Previewing highlighted theme".into());
    frame.render_widget(
        Paragraph::new(message)
            .style(app.theme.style(if diagnostic.is_some() {
                "error"
            } else {
                "muted"
            }))
            .wrap(ratatui::widgets::Wrap { trim: false }),
        parts[2],
    );
    frame.render_widget(
        Paragraph::new("Type to filter · ↑/↓ Ctrl-p/n move · Enter save · Esc cancel")
            .style(app.theme.style("muted")),
        parts[3],
    );
}

fn draw_prompt(frame: &mut Frame, prompt: &Prompt, theme: &Theme) {
    let (title, text, input) = match prompt {
        Prompt::Search(input) => (
            " Search cached mail · Enter apply / Esc cancel ",
            String::new(),
            Some(input.as_str()),
        ),
        Prompt::AttachmentIndex(input) => (
            " Attachment number · Enter / Esc ",
            String::new(),
            Some(input.as_str()),
        ),
        Prompt::AttachmentPath { input, .. } => (
            " Save attachment to · Enter / Esc ",
            String::new(),
            Some(input.as_str()),
        ),
        Prompt::Overwrite { path, .. } => (
            " Replace existing file? ",
            format!("{}\n\ny: overwrite   n / Esc: cancel", path.display()),
            None,
        ),
        Prompt::Trash(message) => (
            " Move this message to Trash? ",
            format!("{}\n\ny: move   n / Esc: cancel", message.subject),
            None,
        ),
        Prompt::Send { preview, .. } => (
            " Send message? · y send / n or Esc keep draft ",
            preview.clone(),
            None,
        ),
    };
    let height = if matches!(prompt, Prompt::Send { .. }) {
        frame.area().height.saturating_sub(4)
    } else if input.is_some() {
        5
    } else {
        8
    };
    let area = popup(frame.area(), 90, height);
    frame.render_widget(Clear, area);
    let block = panel(theme, title, true);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if let Some(input) = input {
        let sanitized = safe_text(input);
        let mut display = sanitized.as_str();
        while display.width() > inner.width.saturating_sub(2) as usize {
            let next = display.chars().next().map_or(0, char::len_utf8);
            display = &display[next..];
        }
        frame.render_widget(
            Paragraph::new(format!("> {display}")).style(theme.style("prompt_input")),
            inner,
        );
        frame.set_cursor_position((inner.x + 2 + display.width() as u16, inner.y));
    } else {
        frame.render_widget(
            Paragraph::new(safe_text(&text))
                .style(theme.style("text"))
                .wrap(ratatui::widgets::Wrap { trim: false }),
            inner,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_colors_apply_to_screens_prompts_help_and_resize_notice() {
        use ratatui::style::Color;
        let custom = r##"
            [palette]
            background = "#010203"
            panel = "#040506"
            foreground = "#070809"
            accent = "#0a0b0c"
            [styles.text]
            fg = "#202122"
            [styles.prompt_input]
            fg = "#303132"
        "##;
        let mut app = crate::app::demo_app().unwrap();
        app.theme = Theme::parse(custom, "custom").unwrap();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
        for view in [View::Folders, View::Threads, View::Message] {
            app.view = view;
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            let buffer = terminal.backend().buffer();
            assert_eq!(buffer[(0, 0)].bg, Color::Rgb(1, 2, 3));
            assert_eq!(buffer[(0, 3)].bg, Color::Rgb(4, 5, 6));
            assert!(
                buffer
                    .content
                    .iter()
                    .any(|cell| cell.fg == Color::Rgb(32, 33, 34))
            );
        }
        app.help = true;
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        assert!(
            terminal
                .backend()
                .buffer()
                .content
                .iter()
                .any(|cell| cell.fg == Color::Rgb(10, 11, 12))
        );
        app.prompt = Some(Prompt::Search("query".into()));
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        assert!(
            terminal
                .backend()
                .buffer()
                .content
                .iter()
                .any(|cell| cell.symbol() == "q" && cell.fg == Color::Rgb(48, 49, 50))
        );
        let mut tiny = ratatui::Terminal::new(ratatui::backend::TestBackend::new(20, 5)).unwrap();
        tiny.draw(|frame| draw(frame, &mut app)).unwrap();
        assert_eq!(tiny.backend().buffer()[(0, 0)].fg, Color::Rgb(10, 11, 12));
        assert_eq!(tiny.backend().buffer()[(0, 0)].bg, Color::Rgb(1, 2, 3));
    }

    #[test]
    fn picker_renders_preview_matches_empty_results_and_cancellation_at_all_sizes() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        for (width, height) in [(120, 40), (70, 24), (40, 12), (10, 3)] {
            let mut app = crate::app::demo_app().unwrap();
            let original = app.theme.clone();
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            app.command("theme.select", None);
            for c in "light".chars() {
                app.theme_picker_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
            }
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            assert_eq!(
                terminal.backend().buffer()[(0, 0)].bg,
                app.theme.style("background").bg.unwrap()
            );
            if width >= 40 {
                let rendered: String = terminal
                    .backend()
                    .buffer()
                    .content
                    .iter()
                    .map(|c| c.symbol())
                    .collect();
                assert!(rendered.contains("Themes"));
                assert!(rendered.contains("Light (light)"));
                assert!(rendered.contains("> light"));
                assert!(
                    terminal
                        .backend()
                        .buffer()
                        .content
                        .iter()
                        .any(|cell| cell.modifier.contains(ratatui::style::Modifier::UNDERLINED))
                );
                let cursor = terminal.get_cursor_position().unwrap();
                assert!(cursor.x < width && cursor.y < height);
            }
            app.theme_picker_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE));
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            if width >= 40 {
                let rendered: String = terminal
                    .backend()
                    .buffer()
                    .content
                    .iter()
                    .map(|c| c.symbol())
                    .collect();
                assert!(rendered.contains("No matching themes"));
            }
            app.theme_picker_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
            assert_eq!(app.theme, original);
        }
    }

    #[test]
    fn renders_decoded_folder_list_and_active_heading() {
        let mut app = crate::app::demo_app().unwrap();
        let encoded = "Gel&APY-schte Elemente";
        app.folders[0].name = encoded.into();
        app.active_folder = encoded.into();
        app.view = View::Threads;
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let rendered: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert_eq!(rendered.matches("Gelöschte Elemente").count(), 2);
        assert!(!rendered.contains("&APY-"));
        assert_eq!(app.active_folder, encoded);
        assert_eq!(app.folders[0].name, encoded);
    }

    #[test]
    fn renders_grouping_collapsed_flat_and_empty_lists() {
        for (width, height) in [(120, 40), (70, 24), (40, 12)] {
            let mut app = crate::app::demo_app().unwrap();
            app.view = View::Threads;
            let mut unknown = app.messages[0].clone();
            unknown.uid = 99;
            unknown.message_id = "unknown".into();
            unknown.references.clear();
            unknown.timestamp = 0;
            unknown.subject = "Undated mail".into();
            let mut messages = app.messages.clone();
            messages.push(unknown);
            app.set_messages(messages);
            app.command("cursor.first", None);
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            let rendered = |terminal: &ratatui::Terminal<ratatui::backend::TestBackend>| -> String {
                terminal
                    .backend()
                    .buffer()
                    .content
                    .iter()
                    .map(|cell| cell.symbol())
                    .collect()
            };
            terminal.draw(|f| draw(f, &mut app)).unwrap();
            assert!(rendered(&terminal).contains("Today"));
            app.command("open", None);
            terminal.draw(|f| draw(f, &mut app)).unwrap();
            assert!(rendered(&terminal).contains("▸ Today"));
            app.command("cursor.last", None);
            terminal.draw(|f| draw(f, &mut app)).unwrap();
            assert!(rendered(&terminal).contains("Undated mail"));
            app.command("threads.toggle_time_grouping", None);
            terminal.draw(|f| draw(f, &mut app)).unwrap();
            assert!(!rendered(&terminal).contains("▸ Today"));
            app.command("open", None);
            terminal.draw(|f| draw(f, &mut app)).unwrap();
            assert!(rendered(&terminal).contains("Undated mail"));
            app.view = View::Threads;
            app.set_messages(vec![]);
            terminal.draw(|f| draw(f, &mut app)).unwrap();
            assert!(rendered(&terminal).contains("No messages"));
        }
    }

    #[test]
    fn renders_normal_narrow_minimum_and_tiny_terminals() {
        for (width, height) in [(120, 40), (70, 24), (40, 12), (10, 3)] {
            let mut app = crate::app::demo_app().unwrap();
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            for view in [View::Folders, View::Threads, View::Message] {
                app.view = view;
                terminal.draw(|f| draw(f, &mut app)).unwrap();
            }
            app.help = true;
            terminal.draw(|f| draw(f, &mut app)).unwrap();
            app.prompt = Some(Prompt::Search("unicode: café 日本語".into()));
            terminal.draw(|f| draw(f, &mut app)).unwrap();
            if width >= 40 {
                let buffer = terminal.backend().buffer();
                let rendered: String = buffer.content.iter().map(|c| c.symbol()).collect();
                assert!(rendered.contains("TERMINAL / EMAIL"));
            }
        }
    }
}
