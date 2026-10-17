use crate::{
    app::{App, Prompt, View},
    model::safe_text,
};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
};
use unicode_width::UnicodeWidthStr;

const BG: Color = Color::Rgb(18, 23, 31);
const PANEL: Color = Color::Rgb(25, 32, 42);
const FG: Color = Color::Rgb(214, 221, 232);
const MUTED: Color = Color::Rgb(127, 146, 165);
const CYAN: Color = Color::Rgb(100, 215, 207);
const GOLD: Color = Color::Rgb(239, 187, 111);
const RED: Color = Color::Rgb(255, 136, 136);

fn panel(title: impl Into<Line<'static>>, active: bool) -> Block<'static> {
    Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if active { CYAN } else { MUTED }))
        .style(Style::default().bg(PANEL).fg(FG))
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    frame.render_widget(Block::default().style(Style::default().bg(BG).fg(FG)), area);
    if area.width < 40 || area.height < 12 {
        frame.render_widget(Paragraph::new("TerminalEmail\n\nPlease resize to at least 40 × 12.\nNavigation and quit keys remain active.").style(Style::default().fg(CYAN)), area);
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
        Span::styled(
            "  TERMINAL / EMAIL",
            Style::default().fg(CYAN).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("    {label}    {}", app.config.account.email),
            Style::default().fg(MUTED),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(title).block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(Style::default().fg(MUTED)),
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
    let mode = if app.prompt.is_some() {
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
                Span::styled(format!(" {mode} "), Style::default().bg(CYAN).fg(BG).bold()),
                Span::styled(
                    format!("  {status}"),
                    Style::default().fg(if app.error { RED } else { FG }),
                ),
            ]),
            Line::styled(
                " j/k move  l open  h back  / search  c compose  ? help  Ctrl-c quit",
                Style::default().fg(MUTED),
            ),
        ]),
        regions[2],
    );
    if app.help {
        help(frame, app);
    }
    if let Some(prompt) = &app.prompt {
        draw_prompt(frame, prompt);
    }
}

fn folders(frame: &mut Frame, app: &App, area: Rect) {
    let items: Vec<_> = app
        .folders
        .iter()
        .map(|folder| {
            let name = safe_text(&folder.name);
            let count = if folder.unread > 0 {
                format!(" ({})", folder.unread)
            } else {
                String::new()
            };
            ListItem::new(Line::from(vec![
                Span::raw(name),
                Span::styled(count, Style::default().fg(GOLD)),
            ]))
        })
        .collect();
    if items.is_empty() {
        frame.render_widget(
            Paragraph::new("No cached folders.\n\nConnecting…\n\nCtrl-r to retry\n? for help")
                .block(panel(" Folders ", app.view == View::Folders)),
            area,
        );
    } else {
        let list = List::new(items)
            .block(panel(" Folders ", app.view == View::Folders))
            .highlight_style(Style::default().bg(Color::Rgb(43, 61, 73)).fg(CYAN).bold())
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
            safe_text(&app.active_folder),
            app.groups.len()
        )
    } else {
        format!(
            " Local search: {} · {} conversations ",
            safe_text(&app.search),
            app.groups.len()
        )
    };
    let block = panel(title, app.view == View::Threads);
    if app.groups.is_empty() {
        frame.render_widget(Paragraph::new("\n  No messages to show.\n\n  Ctrl-r refreshes the mailbox.\n  Esc clears a search.\n  c starts a new message.").style(Style::default().fg(MUTED)).block(block), area);
        return;
    }
    let items = app
        .groups
        .iter()
        .map(|group| {
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
                Style::default().fg(FG).bold()
            } else {
                Style::default().fg(MUTED)
            };
            ListItem::new(vec![
                Line::from(vec![
                    Span::styled(
                        format!("{marker} {} ", if starred { "★" } else { " " }),
                        Style::default().fg(if starred { GOLD } else { CYAN }),
                    ),
                    Span::styled(safe_text(&message.subject), style),
                    Span::styled(count, Style::default().fg(CYAN)),
                ]),
                Line::styled(
                    format!(
                        "     {}   {}",
                        safe_text(&message.from),
                        safe_text(&message.date)
                    ),
                    Style::default().fg(MUTED),
                ),
            ])
        })
        .collect::<Vec<_>>();
    frame.render_stateful_widget(
        List::new(items)
            .block(block)
            .highlight_symbol("› ")
            .highlight_style(Style::default().bg(Color::Rgb(43, 61, 73))),
        area,
        &mut ListState::default().with_selected(Some(app.thread_cursor)),
    );
}

fn reader(frame: &mut Frame, app: &mut App, area: Rect) {
    let count = app.groups.get(app.thread_cursor).map_or(0, Vec::len);
    let block = panel(
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
    let mut text = format!("{}\nFrom: {}\nTo: {}\nDate: {}\n\n{}", message.subject, message.from, message.to, message.date, message.body.as_deref().unwrap_or("Downloading message… Cached headers remain available.\nCtrl-r to retry if the connection failed."));
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
        Paragraph::new(Text::from(lines)).scroll((app.scroll, 0)),
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
            Style::default().fg(CYAN).bold(),
        ),
        Line::raw("  Navigation bindings scroll · back or help closes"),
        Line::default(),
    ];
    for (context, bindings) in &app.keys.bindings {
        lines.push(Line::styled(
            format!("  [{context}]"),
            Style::default().fg(GOLD),
        ));
        for (key, command) in bindings {
            lines.push(Line::raw(format!("  {key:18} {command}")));
        }
        lines.push(Line::default());
    }
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines)
            .scroll((app.help_scroll, 0))
            .block(panel(" Configured bindings ", true)),
        area,
    );
}

fn draw_prompt(frame: &mut Frame, prompt: &Prompt) {
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
    let block = panel(title, true);
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
            Paragraph::new(format!("> {display}")).style(Style::default().fg(CYAN)),
            inner,
        );
        frame.set_cursor_position((inner.x + 2 + display.width() as u16, inner.y));
    } else {
        frame.render_widget(
            Paragraph::new(safe_text(&text)).wrap(ratatui::widgets::Wrap { trim: false }),
            inner,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
