use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
};

use crate::app::{App, HealthState};
use crate::inventory::health::{Health, now_unix};

pub fn draw(f: &mut Frame, area: Rect, app: &App) {
    let Some(state) = app.health_pane.as_ref() else {
        let p = Paragraph::new(Span::styled(
            "no health state — should not happen",
            Style::default().fg(Color::Red),
        ))
        .block(Block::default().borders(Borders::ALL).title("health"));
        f.render_widget(p, area);
        return;
    };

    let title = format!(
        "health › {} businesses ({} pending)",
        state.rows.len(),
        state.pending_count()
    );
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(inner);

    draw_header(f, chunks[0]);
    draw_body(f, chunks[1], state);
}

fn draw_header(f: &mut Frame, area: Rect) {
    // Column header — kept in lockstep with row layout in `draw_body`.
    let line = Line::from(vec![
        Span::styled(
            format!("{:<16}", "BUSINESS"),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            format!("{:<32}", "DOMAIN"),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            format!("{:>4}", "HTTP"),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            format!("{:>6}", "ms"),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            format!("{:>6}", "TLS d"),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        ),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn draw_body(f: &mut Frame, area: Rect, s: &HealthState) {
    let now = now_unix();
    let all_items: Vec<ListItem> = s
        .business_names
        .iter()
        .enumerate()
        .map(|(idx, name)| {
            let row = s.rows.get(idx).and_then(|r| r.as_ref());
            row_to_item(name, row, now)
        })
        .collect();
    let total = all_items.len();
    let viewport = area.height as usize;
    let start = s.scroll.render_start(total, viewport);
    let end = (start + viewport).min(total);
    let visible: Vec<ListItem> = all_items
        .into_iter()
        .skip(start)
        .take(end - start)
        .collect();
    f.render_widget(List::new(visible), area);
}

fn row_to_item<'a>(name: &str, row: Option<&Health>, now: i64) -> ListItem<'a> {
    let biz_cell = format!("{:<16}", truncate(name, 16));

    let Some(h) = row else {
        return ListItem::new(Line::from(vec![
            Span::raw(biz_cell),
            Span::raw("  "),
            Span::styled(
                format!("{:<32}", "(pending)"),
                Style::default().fg(Color::DarkGray),
            ),
        ]));
    };

    let domain_cell = format!("{:<32}", truncate(&h.domain, 32));

    let (http_cell, http_style) = match h.http_status {
        Some(s) => {
            let color = if (200..400).contains(&s) {
                Color::Green
            } else {
                Color::Red
            };
            (
                format!("{:>4}", s),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            )
        }
        None => (format!("{:>4}", "?"), Style::default().fg(Color::DarkGray)),
    };

    let ms_cell = match h.http_ms {
        Some(ms) => format!("{:>6}", ms),
        None => format!("{:>6}", "?"),
    };

    let (tls_cell, tls_style) = match h.tls_days_left(now) {
        Some(d) if d < 0 => (
            format!("{:>6}", "EXPIRED"),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        Some(d) => {
            let color = if d < 14 {
                Color::Red
            } else if d < 30 {
                Color::Yellow
            } else {
                Color::Green
            };
            (
                format!("{:>6}", d),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            )
        }
        None => (format!("{:>6}", "?"), Style::default().fg(Color::DarkGray)),
    };

    let mut spans = vec![
        Span::raw(biz_cell),
        Span::raw("  "),
        Span::raw(domain_cell),
        Span::raw("  "),
        Span::styled(http_cell, http_style),
        Span::raw("  "),
        Span::raw(ms_cell),
        Span::raw("  "),
        Span::styled(tls_cell, tls_style),
    ];
    if let Some(err) = h.error.as_ref() {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            format!("({})", truncate(err, 60)),
            Style::default().fg(Color::Red),
        ));
    }
    ListItem::new(Line::from(spans))
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
