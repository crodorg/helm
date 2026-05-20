use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use crate::app::{App, AgentOutputLine};

pub fn draw(f: &mut Frame, area: Rect, app: &App) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" agent tail  ({} commands) ", app.engine.agent_history.len()))
        .border_style(Style::default().fg(Color::Magenta));

    let mut lines: Vec<Line> = Vec::new();

    if app.engine.agent_history.is_empty() {
        lines.push(Line::from(Span::styled(
            "no agent activity yet — run `helm exec <alias> <cmd>` from another shell",
            Style::default().fg(Color::DarkGray),
        )));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "(this pane only mirrors traffic on helm's control socket — `helm shell`",
            Style::default().fg(Color::DarkGray),
        )));
        lines.push(Line::from(Span::styled(
            " sessions are a separate surface; list them with `helm shell list <alias>`)",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        for (i, entry) in app.engine.agent_history.iter().enumerate() {
            let header = format!(
                "── #{} {} › {} ──",
                i + 1,
                entry.alias,
                entry.cmd
            );
            lines.push(Line::from(Span::styled(
                header,
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
            )));
            for line in &entry.output {
                lines.push(render_line(line));
            }
            if let Some(code) = entry.exit {
                let color = if code == 0 { Color::Green } else { Color::Red };
                lines.push(Line::from(Span::styled(
                    format!("· exit {code}"),
                    Style::default().fg(color).add_modifier(Modifier::ITALIC),
                )));
            } else {
                lines.push(Line::from(Span::styled(
                    "· running…",
                    Style::default().fg(Color::Yellow).add_modifier(Modifier::ITALIC),
                )));
            }
            lines.push(Line::from(""));
        }
    }

    let inner_h = area.height.saturating_sub(2) as usize;
    let total = lines.len();
    let start = app.agent_tail_scroll.render_start(total, inner_h);

    let p = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((start as u16, 0));
    f.render_widget(p, area);
}

fn render_line(l: &AgentOutputLine) -> Line<'static> {
    match l {
        AgentOutputLine::Out(s) => Line::from(Span::raw(s.clone())),
        AgentOutputLine::Err(s) => {
            Line::from(Span::styled(s.clone(), Style::default().fg(Color::Red)))
        }
        AgentOutputLine::System(s) => Line::from(Span::styled(
            format!("· {s}"),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        )),
    }
}
