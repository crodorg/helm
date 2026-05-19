use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
};

use crate::app::App;
use crate::inventory::ports::ListeningSocket;
use crate::inventory::processes::Process;

pub fn draw(f: &mut Frame, area: Rect, app: &App) {
    let Some(state) = app.processes_pane.as_ref() else {
        let p = Paragraph::new(Span::styled(
            "no processes state — should not happen",
            Style::default().fg(Color::Red),
        ))
        .block(Block::default().borders(Borders::ALL).title("processes"));
        f.render_widget(p, area);
        return;
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(65), Constraint::Percentage(35)])
        .split(area);

    draw_processes(f, chunks[0], &state.host_name, state);
    draw_ports(f, chunks[1], state);
}

fn draw_processes(f: &mut Frame, area: Rect, host_name: &str, s: &crate::app::ProcessesState) {
    let title = format!(" processes › {host_name}  (top 20 by CPU) ");
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);

    if let Some(err) = s.error.as_ref() {
        let p = Paragraph::new(Span::styled(
            format!("error: {err}"),
            Style::default().fg(Color::Red),
        ))
        .wrap(Wrap { trim: false });
        f.render_widget(p, inner);
        return;
    }

    let Some(procs) = s.processes.as_ref() else {
        f.render_widget(loading_line("processes"), inner);
        return;
    };

    if procs.is_empty() {
        let p = Paragraph::new(Span::styled(
            "no processes parsed",
            Style::default().fg(Color::DarkGray),
        ));
        f.render_widget(p, inner);
        return;
    }

    let mut lines: Vec<ListItem> = Vec::with_capacity(procs.len() + 1);
    lines.push(ListItem::new(Line::from(Span::styled(
        format!(
            "{:>5} {:>5} {:>7} {:>6} {:<10} {}",
            "%CPU", "%MEM", "RSS_MB", "PID", "USER", "COMMAND"
        ),
        Style::default().fg(Color::DarkGray).add_modifier(Modifier::BOLD),
    ))));
    for p in procs {
        lines.push(ListItem::new(format_proc_row(p)));
    }
    f.render_widget(List::new(lines), inner);
}

fn format_proc_row(p: &Process) -> Line<'static> {
    let rss_mb = (p.rss_kb as f64) / 1024.0;
    let cpu_style = if p.cpu >= 50.0 {
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
    } else if p.cpu >= 10.0 {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default()
    };
    Line::from(vec![
        Span::styled(format!("{:>5.1}", p.cpu), cpu_style),
        Span::raw(" "),
        Span::raw(format!("{:>5.1}", p.mem)),
        Span::raw(" "),
        Span::raw(format!("{:>7.1}", rss_mb)),
        Span::raw(" "),
        Span::raw(format!("{:>6}", p.pid)),
        Span::raw(" "),
        Span::styled(
            format!("{:<10}", truncate(&p.user, 10)),
            Style::default().fg(Color::Cyan),
        ),
        Span::raw(" "),
        Span::raw(p.command.clone()),
    ])
}

fn draw_ports(f: &mut Frame, area: Rect, s: &crate::app::ProcessesState) {
    let count = s.ports.as_ref().map(|p| p.len()).unwrap_or(0);
    let title = format!(" listening sockets  ({count}) ");
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);

    if s.error.is_some() {
        // Error already shown in the processes pane; keep this one quiet.
        return;
    }

    let Some(ports) = s.ports.as_ref() else {
        f.render_widget(loading_line("netstat -na"), inner);
        return;
    };

    if ports.is_empty() {
        let p = Paragraph::new(Span::styled(
            "no listening sockets",
            Style::default().fg(Color::DarkGray),
        ));
        f.render_widget(p, inner);
        return;
    }

    let mut lines: Vec<ListItem> = Vec::with_capacity(ports.len() + 1);
    lines.push(ListItem::new(Line::from(Span::styled(
        format!("{:<6} {}", "PROTO", "LOCAL"),
        Style::default().fg(Color::DarkGray).add_modifier(Modifier::BOLD),
    ))));
    for p in ports {
        lines.push(ListItem::new(format_port_row(p)));
    }
    f.render_widget(List::new(lines), inner);
}

fn format_port_row(p: &ListeningSocket) -> Line<'static> {
    let proto_color = if p.proto.starts_with("tcp") {
        Color::Green
    } else {
        Color::Magenta
    };
    Line::from(vec![
        Span::styled(
            format!("{:<6}", p.proto),
            Style::default().fg(proto_color).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::raw(p.local.clone()),
    ])
}

fn loading_line(label: &str) -> Paragraph<'static> {
    Paragraph::new(Span::styled(
        format!("loading {label}…"),
        Style::default().fg(Color::DarkGray),
    ))
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect()
    }
}
