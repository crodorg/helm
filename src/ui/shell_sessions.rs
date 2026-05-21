//! Sessions pane — tabular view of live `helm shell` tmux sessions across
//! every configured ssh_alias plus the operator's `local` machine.

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
};

use crate::app::{App, ShellSessionsState};

pub fn draw(f: &mut Frame, area: Rect, app: &App) {
    let Some(state) = app.shell_sessions.as_ref() else {
        let p = Paragraph::new(Span::styled(
            "no sessions state — should not happen",
            Style::default().fg(Color::Red),
        ))
        .block(Block::default().borders(Borders::ALL).title("sessions"));
        f.render_widget(p, area);
        return;
    };

    let progress = format!(
        " {}/{} hosts ",
        state.raw.len().min(state.expected),
        state.expected
    );
    let title = format!(" sessions ›{progress}");
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(0)])
        .split(inner);

    draw_header(f, chunks[0], state);
    draw_body(f, chunks[1], state);
}

fn draw_header(f: &mut Frame, area: Rect, _s: &ShellSessionsState) {
    let lines = vec![
        Line::from(vec![
            Span::styled(
                "alias",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("            "),
            Span::styled(
                "target",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(Span::styled(
            "enter open · d ensure detached · r refresh · h/esc back",
            Style::default().fg(Color::DarkGray),
        )),
    ];
    let p = Paragraph::new(lines).wrap(Wrap { trim: false });
    f.render_widget(p, area);
}

fn draw_body(f: &mut Frame, area: Rect, s: &ShellSessionsState) {
    if s.sessions.is_empty() {
        let msg = if s.raw.len() < s.expected {
            "scanning…"
        } else {
            "no live helm-* sessions — open one with `helm shell open <alias>`"
        };
        let p = Paragraph::new(Span::styled(msg, Style::default().fg(Color::DarkGray)))
            .wrap(Wrap { trim: false });
        f.render_widget(p, area);
        return;
    }

    let rows: Vec<ListItem> = s
        .sessions
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let selected = i == s.selected;
            let alias_pad = format!("{:<16}", row.alias);
            if selected {
                // High-contrast inverse video for the entire row so the
                // selection isn't dependent on the terminal's theme palette
                // matching what each column's fg color happens to be.
                let style = Style::default()
                    .fg(Color::Black)
                    .bg(Color::Yellow)
                    .add_modifier(Modifier::BOLD);
                ListItem::new(Line::from(vec![
                    Span::styled(alias_pad, style),
                    Span::styled(" ", style),
                    Span::styled(row.target.clone(), style),
                ]))
            } else {
                let alias_color = if row.alias == crate::tmux::LOCAL_ALIAS {
                    Color::Cyan
                } else {
                    Color::Magenta
                };
                ListItem::new(Line::from(vec![
                    Span::styled(alias_pad, Style::default().fg(alias_color)),
                    Span::raw(" "),
                    Span::raw(row.target.clone()),
                ]))
            }
        })
        .collect();
    let list = List::new(rows);
    f.render_widget(list, area);
}
