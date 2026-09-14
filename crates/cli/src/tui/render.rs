use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};

use super::layout::{self as laid, Kind};
use super::{App, Overlay, Screen};

const ACCENT: Color = Color::Cyan;

pub fn draw(frame: &mut Frame<'_>, app: &mut App) {
    let [body, status] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(frame.area());
    match app.screen {
        Screen::Search => search(frame, app, body),
        Screen::Article => article(frame, app, body),
    }
    status_bar(frame, app, status);
    match app.overlay {
        Overlay::None => {}
        Overlay::Outline(selected) => outline(frame, app, body, selected),
        Overlay::Help => help(frame, body),
    }
}

fn search(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let [input, list] = Layout::vertical([Constraint::Length(3), Constraint::Min(0)]).areas(area);
    let title = format!(" {} ", laid::sanitize(&app.library.meta().title));
    let prompt = Paragraph::new(Line::from(vec![
        Span::styled("› ", Style::new().fg(ACCENT)),
        Span::raw(laid::sanitize(&app.query)),
        Span::styled("▏", Style::new().fg(ACCENT).add_modifier(Modifier::SLOW_BLINK)),
    ]))
    .block(Block::new().borders(Borders::ALL).title(title).border_style(Style::new().fg(Color::DarkGray)));
    frame.render_widget(prompt, input);

    let mut items: Vec<ListItem<'_>> = Vec::new();
    for s in &app.suggestions {
        let mut spans = vec![Span::raw(laid::sanitize(&s.title))];
        if let Some(fragment) = &s.fragment {
            spans.push(Span::styled(format!(" › {}", laid::sanitize(&fragment.replace('_', " "))), Style::new().fg(Color::DarkGray)));
        }
        if let Some(matched) = &s.matched {
            spans.push(Span::styled(format!("  ← {}", laid::sanitize(matched)), Style::new().fg(Color::DarkGray)));
        }
        items.push(ListItem::new(Line::from(spans)));
    }
    if !app.results.is_empty() {
        let width = usize::from(list.width.saturating_sub(4));
        for r in &app.results {
            let summary = truncate(&laid::sanitize(&r.summary), width);
            items.push(ListItem::new(vec![
                Line::from(Span::styled(laid::sanitize(&r.title), Style::new().add_modifier(Modifier::BOLD))),
                Line::from(Span::styled(format!("  {summary}"), Style::new().fg(Color::Gray))),
            ]));
        }
    }
    if items.is_empty() {
        let hint = if app.query.trim().is_empty() {
            "Start typing a title. Tab searches the full text. Ctrl-R opens a random article."
        } else {
            "No titles start with that. Press Enter or Tab to search the full text."
        };
        frame.render_widget(Paragraph::new(Span::styled(hint, Style::new().fg(Color::DarkGray))), inset(list));
        return;
    }
    let highlight = Style::new().bg(Color::DarkGray).add_modifier(Modifier::BOLD);
    let mut state = ListState::default().with_selected(Some(app.selected));
    frame.render_stateful_widget(List::new(items).highlight_style(highlight).highlight_symbol("▌"), inset(list), &mut state);
}

fn article(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let Some(view) = &app.article else { return };
    let width = app.text_width();
    let left = area.width.saturating_sub(width) / 2;
    let text_area = Rect { x: area.x + left, y: area.y, width: width.min(area.width), height: area.height };
    let lines: Vec<Line<'_>> = view
        .laid
        .lines
        .iter()
        .skip(view.scroll)
        .take(usize::from(area.height))
        .map(|line| Line::from(line.spans.iter().map(|s| Span::styled(s.text.as_str(), style_for(s.kind, view.selected_link))).collect::<Vec<_>>()))
        .collect();
    frame.render_widget(Paragraph::new(lines), text_area);
}

fn style_for(kind: Kind, selected: Option<usize>) -> Style {
    let text_style = |s: ok_core::document::Style| {
        let mut style = Style::new();
        if s.bold {
            style = style.add_modifier(Modifier::BOLD);
        }
        if s.italic {
            style = style.add_modifier(Modifier::ITALIC);
        }
        style
    };
    match kind {
        Kind::Text(s) => text_style(s),
        Kind::Link { index, style } => {
            let base = text_style(style).fg(Color::LightBlue).add_modifier(Modifier::UNDERLINED);
            if selected == Some(index) { base.bg(Color::Blue).fg(Color::White) } else { base }
        }
        Kind::Title => Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
        Kind::Heading(2) => Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        Kind::Heading(_) => Style::new().add_modifier(Modifier::BOLD),
        Kind::Marker => Style::new().fg(Color::DarkGray),
        Kind::Label => Style::new().fg(Color::Gray).add_modifier(Modifier::BOLD),
        Kind::Note(s) => text_style(s).fg(Color::Gray),
        Kind::Code => Style::new().fg(Color::Green),
    }
}

fn status_bar(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let mut left = match (&app.screen, &app.article) {
        (Screen::Article, Some(view)) => {
            let total = view.laid.lines.len().max(1);
            let percent = ((view.scroll + app.page_height()).min(total) * 100) / total;
            let section = view.laid.section_at(view.scroll).filter(|s| s.level > 1).map(|s| format!(" › {}", s.heading)).unwrap_or_default();
            format!(" {}{section} · {percent}%", view.title)
        }
        _ => " search".to_string(),
    };
    if !app.status.is_empty() {
        left = format!("{left} · {}", app.status);
    }
    let right = match app.timing {
        Some((what, d)) => format!("{what} {:.1} ms · ? help ", d.as_secs_f64() * 1000.0),
        None => "? help ".into(),
    };
    let room = usize::from(area.width).saturating_sub(laid::display_width(&right));
    let left = truncate(&laid::sanitize(&left), room);
    let pad = room.saturating_sub(laid::display_width(&left));
    let line = Line::from(vec![Span::raw(left), Span::raw(" ".repeat(pad)), Span::styled(right, Style::new().fg(Color::DarkGray))]);
    frame.render_widget(Paragraph::new(line).style(Style::new().bg(Color::Black).fg(Color::Gray)), area);
}

fn outline(frame: &mut Frame<'_>, app: &App, area: Rect, selected: usize) {
    let Some(view) = &app.article else { return };
    let items: Vec<ListItem<'_>> = view
        .laid
        .sections
        .iter()
        .map(|s| ListItem::new(format!("{}{}", "  ".repeat(usize::from(s.level.saturating_sub(1))), laid::sanitize(&s.heading))))
        .collect();
    let popup = centered(area, 60, 80);
    frame.render_widget(Clear, popup);
    let mut state = ListState::default().with_selected(Some(selected));
    let list = List::new(items)
        .block(Block::new().borders(Borders::ALL).title(" Outline · Enter jumps · Esc closes "))
        .highlight_style(Style::new().bg(Color::DarkGray).add_modifier(Modifier::BOLD));
    frame.render_stateful_widget(list, popup, &mut state);
}

fn help(frame: &mut Frame<'_>, area: Rect) {
    let rows = [
        ("Search", ""),
        ("type", "suggest titles as you type"),
        ("↑ ↓  Enter", "choose and open"),
        ("Tab", "search the full text"),
        ("Esc", "back to the article, or clear"),
        ("", ""),
        ("Reading", ""),
        ("j k  ↑ ↓  Space  PgUp PgDn", "scroll"),
        ("g  G", "top, bottom"),
        ("Tab  Shift-Tab  n  N", "select next or previous link"),
        ("Enter", "follow the selected link"),
        ("Backspace  ←  →", "back, forward"),
        ("o", "outline of sections"),
        ("/  s", "search"),
        ("r  Ctrl-R", "random article"),
        ("q  Ctrl-C", "quit"),
    ];
    let lines: Vec<Line<'_>> = rows
        .iter()
        .map(|(k, v)| {
            if v.is_empty() {
                Line::from(Span::styled(*k, Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)))
            } else {
                Line::from(vec![Span::styled(format!("{k:<28}"), Style::new().add_modifier(Modifier::BOLD)), Span::raw(*v)])
            }
        })
        .collect();
    let popup = centered(area, 70, 70);
    frame.render_widget(Clear, popup);
    frame.render_widget(Paragraph::new(lines).block(Block::new().borders(Borders::ALL).title(" Keys · any key closes ")), popup);
}

fn inset(area: Rect) -> Rect {
    Rect { x: area.x + 1, width: area.width.saturating_sub(2), ..area }
}

fn centered(area: Rect, percent_x: u16, percent_y: u16) -> Rect {
    let w = area.width * percent_x / 100;
    let h = area.height * percent_y / 100;
    Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h }
}

fn truncate(text: &str, width: usize) -> String {
    if laid::display_width(text) <= width {
        return text.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if used + w + 1 > width {
            break;
        }
        used += w;
        out.push(c);
    }
    out.push('…');
    out
}
