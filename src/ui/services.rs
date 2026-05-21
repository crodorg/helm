use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
};

use crate::app::App;
use crate::inventory::services::{Service, ServiceState};

pub fn draw(f: &mut Frame, area: Rect, app: &App) {
    let Some(svc_state) = app.services.as_ref() else {
        let p = Paragraph::new(Span::styled(
            "no services state — should not happen",
            Style::default().fg(Color::Red),
        ))
        .block(Block::default().borders(Borders::ALL).title("services"));
        f.render_widget(p, area);
        return;
    };

    let title = format!("services › {}", svc_state.host_name);
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);

    // header (1 line) + body
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(inner);

    draw_header(f, chunks[0], svc_state);
    draw_body(f, chunks[1], svc_state);
}

fn draw_header(f: &mut Frame, area: Rect, s: &crate::app::ServicesState) {
    let summary = if let Some(svc) = s.services.as_ref() {
        let (up, down, fail, trans) = count_states(svc);
        Line::from(vec![
            badge(&format!(" {} ", s.os.label()), Color::Cyan),
            Span::raw("    "),
            badge(" UP ", Color::Green),
            Span::raw(format!(" {up}    ")),
            badge(" DOWN ", Color::DarkGray),
            Span::raw(format!(" {down}    ")),
            badge(" FAIL ", Color::Red),
            Span::raw(format!(" {fail}    ")),
            badge(" TRANS ", Color::Yellow),
            Span::raw(format!(" {trans}")),
        ])
    } else if let Some(err) = s.error.as_ref() {
        Line::from(Span::styled(
            format!("error ({}): {err}", s.os.label()),
            Style::default().fg(Color::Red),
        ))
    } else {
        Line::from(vec![
            Span::styled(
                format!("loading via {}… ", init_system(s.os)),
                Style::default().fg(Color::DarkGray),
            ),
        ])
    };

    f.render_widget(Paragraph::new(summary), area);
}

fn init_system(os: crate::config::OsFamily) -> &'static str {
    match os {
        crate::config::OsFamily::Openbsd => "rcctl",
        crate::config::OsFamily::Debian => "systemctl",
        crate::config::OsFamily::Macos => "launchctl",
    }
}

fn draw_body(f: &mut Frame, area: Rect, s: &crate::app::ServicesState) {
    let Some(services) = s.services.as_ref() else {
        let p = Paragraph::new("").wrap(Wrap { trim: false });
        f.render_widget(p, area);
        return;
    };

    let mut sorted: Vec<&Service> = services.iter().collect();
    sorted.sort_by_key(|s| (state_order(s.state), s.name.clone()));

    let name_width = sorted.iter().map(|s| s.name.len()).max().unwrap_or(0);

    let all_items: Vec<ListItem> = sorted
        .iter()
        .map(|svc| {
            let color = state_color(svc.state);
            let label = format!(" {:<5} ", svc.state.label());
            ListItem::new(Line::from(vec![
                Span::styled(
                    label,
                    Style::default().fg(Color::Black).bg(color).add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(
                    format!("{:<width$}", svc.name, width = name_width),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
            ]))
        })
        .collect();

    let total = all_items.len();
    let viewport = area.height as usize;
    let start = s.scroll.render_start(total, viewport);
    let end = (start + viewport).min(total);
    let visible: Vec<ListItem> = all_items.into_iter().skip(start).take(end - start).collect();
    f.render_widget(List::new(visible), area);
}

fn count_states(svc: &[Service]) -> (usize, usize, usize, usize) {
    let mut up = 0;
    let mut down = 0;
    let mut fail = 0;
    let mut trans = 0;
    for s in svc {
        match s.state {
            ServiceState::Started => up += 1,
            ServiceState::Stopped => down += 1,
            ServiceState::Failed => fail += 1,
            ServiceState::Untracked => trans += 1,
        }
    }
    (up, down, fail, trans)
}

fn state_order(s: ServiceState) -> u8 {
    match s {
        ServiceState::Failed => 0,
        ServiceState::Untracked => 1,
        ServiceState::Started => 2,
        ServiceState::Stopped => 3,
    }
}

fn state_color(s: ServiceState) -> Color {
    match s {
        ServiceState::Started => Color::Green,
        ServiceState::Failed => Color::Red,
        ServiceState::Untracked => Color::Yellow,
        ServiceState::Stopped => Color::DarkGray,
    }
}

fn badge(text: &str, bg: Color) -> Span<'static> {
    Span::styled(
        text.to_string(),
        Style::default().fg(Color::Black).bg(bg).add_modifier(Modifier::BOLD),
    )
}

