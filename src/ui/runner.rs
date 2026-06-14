use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap},
};

use crate::app::{App, InputFocus, OutputLine};

pub fn draw(f: &mut Frame, area: Rect, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(3)])
        .split(area);

    draw_output(f, chunks[0], app);
    draw_input(f, chunks[1], app);

    if app.runner.focus == Some(InputFocus::Password) {
        draw_password_modal(f, area, app);
    }
}

fn draw_output(f: &mut Frame, area: Rect, app: &App) {
    let host = app.selected_host().map(|h| h.name.as_str()).unwrap_or("?");

    let title = format!("runner › {host}");
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let lines: Vec<Line> = app
        .runner
        .output
        .iter()
        .map(|l| match l {
            OutputLine::Out(s) => Line::from(Span::raw(s.clone())),
            OutputLine::Err(s) => {
                Line::from(Span::styled(s.clone(), Style::default().fg(Color::Red)))
            }
            OutputLine::Partial(s) => {
                Line::from(Span::styled(s.clone(), Style::default().fg(Color::Yellow)))
            }
            OutputLine::System(s) => Line::from(Span::styled(
                format!("· {s}"),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            )),
        })
        .collect();

    // Window into the line buffer via skip/take. Previously this used
    // Paragraph with `Wrap { trim: false }` and `.scroll((start, 0))` —
    // but ratatui's Paragraph counts logical lines for `.scroll()` while
    // re-wrapping the rest of the buffer, which produced visually
    // garbled output once you scrolled away from the bottom of a buffer
    // containing any wrapped (long) lines. Switching to a List keeps
    // scroll math 1:1 with visual rows at the cost of long-line wrap.
    let viewport = inner.height as usize;
    let total = lines.len();
    let start = app.runner.scroll.render_start(total, viewport);
    let visible: Vec<ListItem> = lines
        .into_iter()
        .skip(start)
        .take(viewport)
        .map(ListItem::new)
        .collect();
    f.render_widget(List::new(visible), inner);
}

fn draw_input(f: &mut Frame, area: Rect, app: &App) {
    let typing = app.runner.focus == Some(InputFocus::Command);
    let title = if app.runner.running {
        "running… (esc to abort)"
    } else if typing {
        "command (enter to run, esc to back)"
    } else {
        "done (esc to back, r for new command)"
    };

    let style = if typing {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let prompt = if typing { "» " } else { "  " };
    let body = format!("{prompt}{}", app.runner.input);
    let p = Paragraph::new(Line::from(Span::styled(body, style)))
        .block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(p, area);
}

fn draw_password_modal(f: &mut Frame, area: Rect, app: &App) {
    let modal = centered_rect(60, 7, area);
    f.render_widget(Clear, modal);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" password ")
        .border_style(Style::default().fg(Color::Yellow));

    let mask: String = "•".repeat(app.runner.password.chars().count());
    let body = vec![
        Line::from(Span::styled(
            "remote prompt detected — enter password",
            Style::default().fg(Color::Yellow),
        )),
        Line::from(""),
        Line::from(vec![
            Span::raw("» "),
            Span::styled(mask, Style::default().add_modifier(Modifier::BOLD)),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "[enter] submit   [esc] cancel",
            Style::default().fg(Color::DarkGray),
        )),
    ];
    let p = Paragraph::new(body)
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
