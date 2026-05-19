use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

use crate::app::{App, Mode};
use crate::help::{bindings_for, mode_title};

pub fn draw(f: &mut Frame, area: Rect, app: &App) {
    let origin = app.help_origin.unwrap_or(Mode::Browse);
    let bindings = bindings_for(origin, app.runner.focus);
    let title = mode_title(origin);

    // Height: 4 chrome rows (borders + header line + spacer + footer line) +
    // one line per binding, capped to area height.
    let inner_rows = bindings.len() as u16 + 4;
    let modal = centered_rect(60, inner_rows.min(area.height), area);
    f.render_widget(Clear, modal);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" keys › {title} "))
        .border_style(Style::default().fg(Color::Cyan));

    let max_key = bindings.iter().map(|b| b.key.len()).max().unwrap_or(0);

    let mut lines = vec![
        Line::from(Span::styled(
            "press [?] or [esc] to close",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(""),
    ];

    for b in bindings {
        lines.push(Line::from(vec![
            Span::raw(" "),
            Span::styled(
                format!("[{}]", b.key),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" ".repeat(max_key.saturating_sub(b.key.len()) + 2)),
            Span::raw(b.action),
        ]));
    }

    let p = Paragraph::new(lines)
        .block(block)
        .alignment(Alignment::Left)
        .wrap(Wrap { trim: false });
    f.render_widget(p, modal);
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
