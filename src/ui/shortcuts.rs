use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

use crate::app::App;

pub fn draw(f: &mut Frame, area: Rect, app: &App) {
    let host_name = app
        .selected_host()
        .map(|h| h.name.as_str())
        .unwrap_or("?");

    let shortcuts = app.applicable_shortcuts();
    let modal = centered_rect(70, (shortcuts.len() as u16).saturating_add(6).min(area.height), area);
    f.render_widget(Clear, modal);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" shortcuts › {host_name} "))
        .border_style(Style::default().fg(Color::Cyan));

    let mut lines = vec![
        Line::from(Span::styled(
            "press a shortcut key — runs through Runner (esc to cancel)",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(""),
    ];

    if shortcuts.is_empty() {
        lines.push(Line::from(Span::styled(
            "no shortcuts apply to this host",
            Style::default().fg(Color::Red),
        )));
    } else {
        let max_label = shortcuts.iter().map(|s| s.label.len()).max().unwrap_or(0);
        for s in &shortcuts {
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" [{}] ", s.key),
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(
                    format!("{:<width$}", s.label, width = max_label),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(
                    truncate(&s.cmd, 60),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
        }
    }

    let p = Paragraph::new(lines)
        .block(block)
        .alignment(Alignment::Left)
        .wrap(Wrap { trim: false });
    f.render_widget(p, modal);
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

fn centered_rect(percent_x: u16, height: u16, r: Rect) -> Rect {
    let popup_width = r.width * percent_x / 100;
    let x = r.x + (r.width - popup_width) / 2;
    let y = r.y + (r.height.saturating_sub(height)) / 2;
    Rect {
        x,
        y,
        width: popup_width,
        height: height.min(r.height),
    }
}
